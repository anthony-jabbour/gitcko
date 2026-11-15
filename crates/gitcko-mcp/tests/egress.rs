// SPDX-License-Identifier: AGPL-3.0-or-later

//! Egress controls for `gitcko_review`: unacknowledged remotes refuse before the engine;
//! acknowledged remotes may run (positive control). Repository `[models]` never starts a run.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gitcko_mcp::{GitckoServer, ProviderMap, ReviewJob, Script};
use gitcko_model::{EventStream, FakeProvider, ModelProvider, Request};
use gitcko_protocol::review::Review;
use gitcko_protocol::review::ReviewStatus;
use gitcko_review::Outcome;
use gitcko_test_support::{TempDir, empty_repo, git, workspace_root};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::json;

fn review_args() -> serde_json::Map<String, serde_json::Value> {
    json!({ "base": "base", "head": "HEAD" })
        .as_object()
        .cloned()
        .unwrap()
}

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

fn fixture_repo() -> TempDir {
    let repo = empty_repo("mcp-egress");
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

fn home_env(config_body: &str) -> (TempDir, BTreeMap<String, String>) {
    let home = TempDir::new("mcp-egress-home");
    let home_path = home.path().canonicalize().unwrap();
    let config = home_path.join("config.toml");
    std::fs::write(&config, config_body).unwrap();
    let cache = home_path.join("cache");
    std::fs::create_dir(&cache).unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), home_path.display().to_string()),
        ("APPDATA".into(), home_path.display().to_string()),
        (
            "LOCALAPPDATA".into(),
            home_path.join("local").display().to_string(),
        ),
        ("GITCKO_CONFIG".into(), config.display().to_string()),
        ("GITCKO_CACHE_DIR".into(), cache.display().to_string()),
    ]);
    (home, env)
}

fn complete_review() -> Review {
    let path = workspace_root().join("crates/gitcko-protocol/tests/data/review-full.json");
    let mut review: Review = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    review.status = ReviewStatus::Complete;
    review
}

fn structured_code(result: &CallToolResult) -> String {
    result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("code"))
        .and_then(|code| code.as_str())
        .unwrap_or("")
        .to_string()
}

fn structured_message(result: &CallToolResult) -> String {
    result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("message"))
        .and_then(|code| code.as_str())
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn unacknowledged_remote_never_reaches_the_script() {
    let repo = fixture_repo();
    let remote = r#"
[models.bindings.smart]
provider = "openrouter"
model = "anthropic/example-1"
data_policy = "zdr"

[models.bindings.judge]
provider = "openrouter"
model = "anthropic/example-1"
data_policy = "zdr"

[models.roles]
judge = "judge"
"#;
    let (_closed_home, mut closed) = home_env(remote);
    closed.insert("OPENROUTER_API_KEY".into(), "not-a-key".into());
    let reached = Arc::new(AtomicBool::new(false));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let probe = reached.clone();
    let script: Script = Arc::new(move |_job: &ReviewJob| {
        probe.store(true, Ordering::SeqCst);
        Ok(Outcome {
            review: complete_review(),
            dropped: Vec::new(),
            records: Vec::new(),
            events: Vec::new(),
        })
    });
    let server =
        GitckoServer::with_script(repo.path().to_path_buf(), closed, script, stderr.clone());
    let (server_io, client_io) = tokio::io::duplex(256 * 1024);
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client");
    let mut params = CallToolRequestParams::new("gitcko_review");
    params.arguments = Some(review_args());
    let denied = client.peer().call_tool(params).await.expect("call");
    assert!(!reached.load(Ordering::SeqCst));
    assert_eq!(structured_code(&denied), "CAPABILITY_UNAVAILABLE");
    assert_eq!(
        structured_message(&denied),
        "run gitcko setup --accept-remote openrouter"
    );
    let err = String::from_utf8(stderr.lock().unwrap().clone()).unwrap();
    assert!(
        !err.contains("selected code is sent"),
        "an unacknowledged remote is not disclosed: {err}"
    );
    assert!(!structured_message(&denied).contains("not-a-key"));
    drop(_closed_home);
    drop(client);
    server_task.abort();
}

#[tokio::test]
async fn acknowledged_remote_reaches_the_script_with_local_only_false() {
    let repo = fixture_repo();
    let remote = r#"
[models.bindings.smart]
provider = "openrouter"
model = "anthropic/example-1"
data_policy = "zdr"

[models.bindings.judge]
provider = "openrouter"
model = "anthropic/example-1"
data_policy = "zdr"

[models.roles]
judge = "judge"

[privacy]
remote_acknowledged = ["openrouter"]
"#;
    let (open_home, mut open) = home_env(remote);
    open.insert("OPENROUTER_API_KEY".into(), "not-a-key".into());
    let reached = Arc::new(AtomicBool::new(false));
    let local_only = Arc::new(Mutex::new(None));
    let flag = local_only.clone();
    let probe = reached.clone();
    let script: Script = Arc::new(move |job: &ReviewJob| {
        probe.store(true, Ordering::SeqCst);
        *flag.lock().unwrap() = Some(job.local_only);
        Ok(Outcome {
            review: complete_review(),
            dropped: Vec::new(),
            records: Vec::new(),
            events: Vec::new(),
        })
    });
    let server = GitckoServer::with_script(
        repo.path().to_path_buf(),
        open,
        script,
        Arc::new(Mutex::new(Vec::new())),
    );
    let (server_io, client_io) = tokio::io::duplex(256 * 1024);
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client");
    let mut params = CallToolRequestParams::new("gitcko_review");
    params.arguments = Some(review_args());
    let _ = client.peer().call_tool(params).await.expect("call");
    assert!(reached.load(Ordering::SeqCst));
    assert_eq!(*local_only.lock().unwrap(), Some(false));
    drop(open_home);
    drop(client);
    server_task.abort();
}

#[tokio::test]
async fn local_binding_reaches_the_script_with_local_only_true() {
    let repo = fixture_repo();
    let local = r#"
[models.bindings.smart]
provider = "ollama"
model = "fixture:1"
endpoint = "http://127.0.0.1:11434"
context = 131072

[models.bindings.judge]
provider = "ollama"
model = "fixture:1"
endpoint = "http://127.0.0.1:11434"
context = 131072
"#;
    let (_local_home, env) = home_env(local);
    let local_only = Arc::new(Mutex::new(None));
    let reached = Arc::new(AtomicBool::new(false));
    let flag = local_only.clone();
    let probe = reached.clone();
    let script: Script = Arc::new(move |job: &ReviewJob| {
        probe.store(true, Ordering::SeqCst);
        *flag.lock().unwrap() = Some(job.local_only);
        Ok(Outcome {
            review: complete_review(),
            dropped: Vec::new(),
            records: Vec::new(),
            events: Vec::new(),
        })
    });
    let server = GitckoServer::with_script(
        repo.path().to_path_buf(),
        env,
        script,
        Arc::new(Mutex::new(Vec::new())),
    );
    let (server_io, client_io) = tokio::io::duplex(256 * 1024);
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client");
    let mut params = CallToolRequestParams::new("gitcko_review");
    params.arguments = Some(review_args());
    let _ = client.peer().call_tool(params).await.expect("call");
    assert!(reached.load(Ordering::SeqCst));
    assert_eq!(*local_only.lock().unwrap(), Some(true));
    drop(_local_home);
    drop(client);
    server_task.abort();
}

#[tokio::test]
async fn repository_models_are_rejected_and_never_reach_the_script() {
    let repo = fixture_repo();
    std::fs::create_dir_all(repo.path().join(".gitcko")).unwrap();
    std::fs::write(
        repo.path().join(".gitcko/config.toml"),
        "[models.bindings.smart]\nprovider = \"ollama\"\nmodel = \"x\"\n",
    )
    .unwrap();
    let (_repo_home, env) = home_env(
        r#"
[models.bindings.smart]
provider = "ollama"
model = "fixture:1"
endpoint = "http://127.0.0.1:11434"
context = 131072

[models.bindings.judge]
provider = "ollama"
model = "fixture:1"
endpoint = "http://127.0.0.1:11434"
context = 131072
"#,
    );
    let reached = Arc::new(AtomicBool::new(false));
    let probe = reached.clone();
    let script: Script = Arc::new(move |_job: &ReviewJob| {
        probe.store(true, Ordering::SeqCst);
        Ok(Outcome {
            review: complete_review(),
            dropped: Vec::new(),
            records: Vec::new(),
            events: Vec::new(),
        })
    });
    let server = GitckoServer::with_script(
        repo.path().to_path_buf(),
        env,
        script,
        Arc::new(Mutex::new(Vec::new())),
    );
    let (server_io, client_io) = tokio::io::duplex(256 * 1024);
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client");
    let mut params = CallToolRequestParams::new("gitcko_review");
    params.arguments = Some(review_args());
    let denied = client.peer().call_tool(params).await.expect("call");
    assert_eq!(structured_code(&denied), "INVALID_REQUEST");
    assert!(!reached.load(Ordering::SeqCst));
    drop(_repo_home);
    drop(client);
    server_task.abort();
}
struct Boom {
    hit: Arc<AtomicBool>,
}

impl ModelProvider for Boom {
    fn stream(&self, _: Request, _: tokio_util::sync::CancellationToken) -> EventStream {
        self.hit.store(true, Ordering::SeqCst);
        panic!("remote provider was reached");
    }
}

fn providers(names: &[(&str, Arc<dyn ModelProvider>)]) -> ProviderMap {
    names
        .iter()
        .map(|(name, provider)| ((*name).to_string(), Arc::clone(provider)))
        .collect()
}

async fn review(server: GitckoServer) -> CallToolResult {
    let (server_io, client_io) = tokio::io::duplex(1024 * 1024);
    let task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client");
    let mut params = CallToolRequestParams::new("gitcko_review");
    params.arguments = Some(review_args());
    let result = client.peer().call_tool(params).await.expect("call");
    drop(client);
    task.abort();
    result
}

#[tokio::test]
async fn a_local_primary_never_calls_its_remote_fallback_and_a_remote_primary_is_called() {
    let repo = fixture_repo();
    let mixed = r#"
[models.bindings.smart]
provider = "ollama"
model = "fixture:1"
endpoint = "http://127.0.0.1:11434"
context = 131072

[models.bindings.remote]
provider = "openrouter"
model = "example/model-1"

[models.roles]
review = "smart"
judge = "smart"

[models.fallbacks]
smart = ["remote"]

[privacy]
remote_acknowledged = ["openrouter"]
"#;
    let (_mixed_home, mut env) = home_env(mixed);
    env.insert("OPENROUTER_API_KEY".into(), "canary-key".into());
    let local = Arc::new(FakeProvider::new(Vec::new()));
    let hit = Arc::new(AtomicBool::new(false));
    let map = providers(&[
        ("smart", Arc::clone(&local) as Arc<dyn ModelProvider>),
        (
            "remote",
            Arc::new(Boom {
                hit: Arc::clone(&hit),
            }),
        ),
    ]);
    let result = review(GitckoServer::with_providers(
        repo.path().to_path_buf(),
        env,
        map,
        Arc::new(Mutex::new(Vec::new())),
    ))
    .await;
    assert!(local.calls() > 0, "the on-device binding was not called");
    assert!(
        !hit.load(Ordering::SeqCst),
        "the remote fallback was called"
    );
    let rendered = format!("{result:?}");
    assert!(!rendered.contains("canary-key"), "{rendered}");

    let remote_only = r#"
[models.bindings.smart]
provider = "openrouter"
model = "example/model-1"

[models.roles]
review = "smart"
judge = "smart"

[privacy]
remote_acknowledged = ["openrouter"]
"#;
    let (_remote_home, mut env) = home_env(remote_only);
    env.insert("OPENROUTER_API_KEY".into(), "canary-key".into());
    let remote = Arc::new(FakeProvider::new(Vec::new()));
    let map = providers(&[("smart", Arc::clone(&remote) as Arc<dyn ModelProvider>)]);
    let result = review(GitckoServer::with_providers(
        repo.path().to_path_buf(),
        env,
        map,
        Arc::new(Mutex::new(Vec::new())),
    ))
    .await;
    assert!(
        remote.calls() > 0,
        "an acknowledged remote primary was not called: {result:?}"
    );
    let rendered = format!("{result:?}");
    assert!(!rendered.contains("canary-key"), "{rendered}");
}
