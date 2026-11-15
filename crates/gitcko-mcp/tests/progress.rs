// SPDX-License-Identifier: AGPL-3.0-or-later

//! Progress uses real stages. Cancelling the request stops further model calls and
//! stores a canceled review.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use gitcko_mcp::{GitckoServer, ProviderMap};
use gitcko_model::{ErrorKind, EventStream, FakeProvider, ModelProvider, ProviderError, Request};
use gitcko_protocol::review::ReviewStatus;
use gitcko_storage::ReviewStore;
use gitcko_test_support::{TempDir, empty_repo, git};
use rmcp::ClientHandler;
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, ClientRequest, ProgressNotificationParam,
};
use rmcp::service::{NotificationContext, PeerRequestOptions, RoleClient};

const LOCAL: &str = r#"
[models.bindings.smart]
provider = "ollama"
model = "fixture:1"
endpoint = "http://127.0.0.1:11434"
context = 131072

[models.roles]
review = "smart"
judge = "smart"
"#;

fn commit(dir: &std::path::Path, message: &str) {
    assert!(git(dir).args(["add", "-A"]).status().unwrap().success());
    assert!(
        git(dir)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@fixture.invalid",
                "commit",
                "-q",
                "-m",
                message,
            ])
            .status()
            .unwrap()
            .success()
    );
}

fn repo() -> TempDir {
    let repo = empty_repo("mcp-progress");
    let root = repo.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), b"[package]\nname = \"fixture\"\n").unwrap();
    std::fs::write(root.join("src/lib.rs"), b"pub fn kept() {}\n").unwrap();
    commit(root, "base");
    assert!(
        git(root)
            .args(["branch", "base"])
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(root.join("src/lib.rs"), b"pub fn kept() { let _ = 1; }\n").unwrap();
    commit(root, "head");
    repo
}

fn home(config: &str) -> (TempDir, BTreeMap<String, String>) {
    let home = TempDir::new("mcp-progress-home");
    let home_path = home.path().canonicalize().unwrap();
    let config_path = home_path.join("config.toml");
    std::fs::write(&config_path, config).unwrap();
    let cache = home_path.join("cache");
    std::fs::create_dir(&cache).unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), home_path.display().to_string()),
        ("APPDATA".into(), home_path.display().to_string()),
        (
            "LOCALAPPDATA".into(),
            home_path.join("local").display().to_string(),
        ),
        ("GITCKO_CONFIG".into(), config_path.display().to_string()),
        ("GITCKO_CACHE_DIR".into(), cache.display().to_string()),
    ]);
    (home, env)
}

fn review_args() -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({"base": "base", "head": "HEAD"})
        .as_object()
        .unwrap()
        .clone()
}

type StageNote = (f64, Option<f64>, String);

struct Notes {
    events: Arc<Mutex<Vec<StageNote>>>,
}

impl ClientHandler for Notes {
    fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) -> impl std::future::Future<Output = ()> + Send {
        self.events.lock().unwrap().push((
            params.progress,
            params.total,
            params.message.unwrap_or_default(),
        ));
        std::future::ready(())
    }
}

struct Gate {
    calls: Arc<AtomicUsize>,
}

impl ModelProvider for Gate {
    fn stream(&self, _: Request, cancel: tokio_util::sync::CancellationToken) -> EventStream {
        let calls = Arc::clone(&self.calls);
        futures_util::stream::once(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            cancel.cancelled().await;
            Err(ProviderError::new(ErrorKind::Canceled))
        })
        .boxed()
    }
}

#[tokio::test]
async fn a_review_reports_real_stages_and_not_a_made_up_percentage() {
    let repo = repo();
    let (_home, env) = home(LOCAL);
    let provider = Arc::new(FakeProvider::new(Vec::new()));
    let mut map = ProviderMap::new();
    map.insert("smart".into(), provider.clone());
    let server = GitckoServer::with_providers(
        repo.path().to_path_buf(),
        env,
        map,
        Arc::new(Mutex::new(Vec::new())),
    );
    let notes = Notes {
        events: Arc::new(Mutex::new(Vec::new())),
    };
    let seen = Arc::clone(&notes.events);
    let (server_io, client_io) = tokio::io::duplex(1024 * 1024);
    let task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve");
        let _ = running.waiting().await;
    });
    let client = notes.serve(client_io).await.expect("client");
    let params = CallToolRequestParams::new("gitcko_review").with_arguments(review_args());
    let result = client.peer().call_tool(params).await.expect("call");
    assert_ne!(result.is_error, Some(true));
    assert!(provider.calls() > 0, "the review never called the model");
    let events = seen.lock().unwrap().clone();
    assert!(!events.is_empty(), "no progress for a review that ran");
    let mut previous = 0.0;
    for (progress, total, message) in &events {
        assert!(*progress > previous, "{events:?}");
        previous = *progress;
        assert!(
            total.is_none(),
            "progress must not invent a total: {events:?}"
        );
        assert!(message.starts_with("stage: "), "{message}");
        assert!(!message.contains('%'), "{message}");
    }
    drop(client);
    task.abort();
}

#[tokio::test]
async fn cancelling_a_review_stores_canceled_and_stops_model_calls() {
    let repo = repo();
    let (_home, env) = home(LOCAL);
    let calls = Arc::new(AtomicUsize::new(0));
    let mut map = ProviderMap::new();
    map.insert(
        "smart".into(),
        Arc::new(Gate {
            calls: Arc::clone(&calls),
        }),
    );
    let server = GitckoServer::with_providers(
        repo.path().to_path_buf(),
        env.clone(),
        map,
        Arc::new(Mutex::new(Vec::new())),
    );
    let (server_io, client_io) = tokio::io::duplex(1024 * 1024);
    let task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client");
    let params = CallToolRequestParams::new("gitcko_review").with_arguments(review_args());
    let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
    let handle = client
        .peer()
        .send_cancellable_request(request, PeerRequestOptions::no_options())
        .await
        .expect("send");
    let started = tokio::time::Instant::now() + Duration::from_secs(10);
    while calls.load(Ordering::SeqCst) == 0 && tokio::time::Instant::now() < started {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        calls.load(Ordering::SeqCst) >= 1,
        "the model call never started"
    );
    handle.cancel(None).await.expect("cancel");
    let cache = env.get("GITCKO_CACHE_DIR").unwrap();
    let key = gitcko_mcp::root_id(repo.path()).unwrap();
    let store = ReviewStore::new(cache);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut stored = None;
    while tokio::time::Instant::now() < deadline {
        if let Ok(list) = store.list(&key)
            && let Some(item) = list
                .into_iter()
                .find(|item| item.status == ReviewStatus::Canceled)
        {
            stored = Some(item.review_id);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let id = stored.expect("the canceled review was not stored");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a later model call ran after cancel"
    );
    let review = store.get(&key, &id).unwrap();
    assert_eq!(review.status, ReviewStatus::Canceled);
    drop(client);
    task.abort();
}
