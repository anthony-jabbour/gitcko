// SPDX-License-Identifier: AGPL-3.0-or-later

//! The four Server A tools: schemas, path checks, stored reviews, and incomplete text.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use gitcko_domain::ReviewId;
use gitcko_protocol::review::{Review, ReviewStatus};
use gitcko_review::Outcome;
use gitcko_storage::{ReviewStore, RunningMarker};
use gitcko_test_support::{TempDir, empty_repo, git, workspace_root};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use serde_json::{Value, json};

const LOCAL: &str = r#"
[models.bindings.smart]
provider = "ollama"
model = "fixture:1"
endpoint = "http://127.0.0.1:11434"
context = 131072
"#;

struct World {
    repo: TempDir,
    _home: TempDir,
    env: std::collections::BTreeMap<String, String>,
    cache: std::path::PathBuf,
}

fn prepare(config: &str) -> World {
    let repo = empty_repo("mcp-tools");
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

    let home = TempDir::new("mcp-home");
    let home_path = home.path().canonicalize().unwrap();
    let cache = home_path.join("cache");
    std::fs::create_dir(&cache).unwrap();
    let config_path = home_path.join("config.toml");
    std::fs::write(&config_path, config).unwrap();
    let env = std::collections::BTreeMap::from([
        ("HOME".into(), home_path.display().to_string()),
        ("GITCKO_CONFIG".into(), config_path.display().to_string()),
        ("GITCKO_CACHE_DIR".into(), cache.display().to_string()),
    ]);
    World {
        repo,
        _home: home,
        env,
        cache,
    }
}

fn commit(dir: &Path, message: &str) {
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

fn sha(dir: &Path, rev: &str) -> String {
    let output = git(dir).args(["rev-parse", rev]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn sample() -> Review {
    let path = workspace_root().join("crates/gitcko-protocol/tests/data/review-full.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

struct Made {
    server: gitcko_mcp::GitckoServer,
    stderr: Arc<Mutex<Vec<u8>>>,
    calls: Arc<AtomicUsize>,
}

fn scripted(world: &World, status: ReviewStatus, findings: usize) -> Made {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_run = Arc::clone(&calls);
    let template = sample();
    let stderr = Arc::new(Mutex::new(Vec::new()));
    let server = gitcko_mcp::GitckoServer::with_script(
        world.repo.path().to_path_buf(),
        world.env.clone(),
        Arc::new(move |job: &gitcko_mcp::ReviewJob| {
            calls_run.fetch_add(1, Ordering::SeqCst);
            let mut review = template.clone();
            review.status = status;
            review.reason_codes = if status == ReviewStatus::Complete {
                Vec::new()
            } else {
                vec!["budget_exhausted".into()]
            };
            review.score = Some(4);
            if findings == 0 {
                review.findings.clear();
            } else if findings >= 2 {
                let mut extra = review.findings[0].clone();
                extra.id = "f_0000000000000002".into();
                extra.severity = gitcko_protocol::review::Severity::P0;
                extra.suggestion = None;
                review.findings.push(extra);
            }
            let _ = job;
            Ok(Outcome {
                review,
                dropped: Vec::new(),
                records: Vec::new(),
                events: Vec::new(),
            })
        }),
        Arc::clone(&stderr),
    );
    Made {
        server,
        stderr,
        calls,
    }
}

async fn drive(
    server: gitcko_mcp::GitckoServer,
    calls: &[(&str, Value)],
) -> Vec<Result<CallToolResult, String>> {
    let (server_io, client_io) = tokio::io::duplex(1024 * 1024);
    let task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_io).await.expect("client");
    let mut out = Vec::new();
    for (name, args) in calls {
        let mut params = CallToolRequestParams::new(name.to_string());
        if let Value::Object(map) = args {
            params.arguments = Some(map.clone());
        }
        out.push(
            client
                .peer()
                .call_tool(params)
                .await
                .map_err(|err| err.to_string()),
        );
    }
    drop(client);
    task.abort();
    out
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect()
}

fn value_of(result: &CallToolResult) -> Value {
    result.structured_content.clone().unwrap_or(Value::Null)
}

fn review_args(extra: Value) -> Value {
    let mut args = json!({"base": "base", "head": "HEAD"});
    if let (Some(obj), Some(more)) = (args.as_object_mut(), extra.as_object()) {
        for (key, value) in more {
            obj.insert(key.clone(), value.clone());
        }
    }
    args
}

#[tokio::test]
async fn a_local_review_reaches_the_engine_and_query_tools_do_not() {
    let world = prepare(LOCAL);
    let made = scripted(&world, ReviewStatus::Complete, 0);
    let mut results = drive(
        made.server,
        &[
            ("gitcko_query_repo", json!({"query": "canary_query"})),
            (
                "gitcko_review",
                review_args(json!({"tier": "base", "paths": ["src/lib.rs"]})),
            ),
        ],
    )
    .await;
    assert!(results[0].is_err(), "query tools are not routed");
    let ok = results.remove(1).unwrap();
    assert_eq!(ok.is_error, Some(false));
    assert_eq!(made.calls.load(Ordering::SeqCst), 1);
    let text = text_of(&ok);
    assert!(text.starts_with("status: complete"), "{text}");
    assert!(text.contains("No issues found"), "{text}");
    let value = value_of(&ok);
    assert_eq!(value["status"], "complete");
    assert_eq!(value["tier"], "base");
    assert_eq!(value["paths"], json!(["src/lib.rs"]));
    assert_eq!(value["untrusted_content"], true);
    assert_eq!(value["schema_version"], "gitcko.mcp/1");
    let err = String::from_utf8(made.stderr.lock().unwrap().clone()).unwrap();
    assert!(err.contains("local: code stays on this machine"), "{err}");
    assert!(!text.contains("code stays on this machine"), "{text}");
}

#[tokio::test]
async fn rejected_arguments_do_not_start_a_review_and_a_good_path_does() {
    let world = prepare(LOCAL);
    let outside = TempDir::new("canary_outside");
    std::os::unix::fs::symlink(outside.path(), world.repo.path().join("canary_link")).unwrap();
    let made = scripted(&world, ReviewStatus::Partial, 0);
    let calls = [
        ("gitcko_review", json!({"model": "canary_model"})),
        ("gitcko_review", review_args(json!({"tier": "nope"}))),
        ("gitcko_review", review_args(json!({"tier": "plus"}))),
        (
            "gitcko_review",
            review_args(json!({"instructions": "canary_note"})),
        ),
        (
            "gitcko_review",
            review_args(json!({"paths": ["/tmp/canary_abs"]})),
        ),
        (
            "gitcko_review",
            review_args(json!({"paths": ["../canary_escape"]})),
        ),
        (
            "gitcko_review",
            review_args(json!({"paths": ["canary_link"]})),
        ),
        (
            "gitcko_review",
            review_args(json!({"paths": ["src/lib.rs"]})),
        ),
    ];
    let results = drive(made.server, &calls).await;
    assert_eq!(
        made.calls.load(Ordering::SeqCst),
        1,
        "only the last call runs"
    );
    let codes = [
        "INVALID_REQUEST",
        "INVALID_REQUEST",
        "CAPABILITY_UNAVAILABLE",
        "CAPABILITY_UNAVAILABLE",
        "INVALID_REQUEST",
        "INVALID_REQUEST",
        "INVALID_REQUEST",
    ];
    for (result, code) in results.iter().take(7).zip(codes) {
        let result = result.as_ref().unwrap();
        assert_eq!(result.is_error, Some(true), "{code}");
        let value = value_of(result);
        assert_eq!(value["code"], code, "{value}");
        let text = text_of(result);
        assert!(!text.contains("canary"), "{text}");
        assert!(!value.to_string().contains("canary"), "{value}");
    }
    let ok = results[7].as_ref().unwrap();
    assert_eq!(ok.is_error, Some(false));
    let text = text_of(ok);
    assert!(
        text.starts_with("INCOMPLETE review (budget_exhausted)"),
        "{text}"
    );
    assert!(!text.contains("No issues found"), "{text}");
    let value = value_of(ok);
    assert_eq!(value["status"], "partial");
    assert_eq!(value["score"], Value::Null);
    assert_eq!(value["reason_codes"], json!(["budget_exhausted"]));
}

#[tokio::test]
async fn a_moving_branch_stays_bound_to_the_shas_from_the_call() {
    let world = prepare(LOCAL);
    let head = sha(world.repo.path(), "HEAD");
    let made = scripted(&world, ReviewStatus::Complete, 1);
    let results = drive(made.server, &[("gitcko_review", review_args(json!({})))]).await;
    let value = value_of(results[0].as_ref().unwrap());
    assert_eq!(value["repository"]["head_sha"], head);
    assert_ne!(value["repository"]["base_sha"], head);
    let review_id = value["review_id"].as_str().unwrap().to_string();
    std::fs::write(
        world.repo.path().join("src/lib.rs"),
        b"pub fn kept() { let _ = 2; }\n",
    )
    .unwrap();
    commit(world.repo.path(), "later");
    assert_ne!(sha(world.repo.path(), "HEAD"), head);
    let made = scripted(&world, ReviewStatus::Complete, 0);
    let got = drive(
        made.server,
        &[("gitcko_get_review", json!({"review_id": review_id}))],
    )
    .await;
    let value = value_of(got[0].as_ref().unwrap());
    assert_eq!(value["repository"]["head_sha"], head);
    assert_eq!(value["findings"][0]["fix_package"], Value::Null);
    assert_eq!(
        value["findings"][0]["occurrence_id"],
        format!("f_0123456789abcdef:{review_id}")
    );
    assert_eq!(value["findings"][0]["severity"], "P2");
    assert_eq!(value["score"], json!(4));
    assert_eq!(value["status"], "complete");
}

#[tokio::test]
async fn status_get_and_list_read_the_store_and_addressed_follows_a_later_complete_review() {
    let world = prepare(LOCAL);
    let made = scripted(&world, ReviewStatus::Complete, 2);
    let stderr = Arc::clone(&made.stderr);
    let results = drive(made.server, &[("gitcko_review", review_args(json!({})))]).await;
    let stored = value_of(results[0].as_ref().unwrap());
    let review_id = stored["review_id"].as_str().unwrap().to_string();
    let base_sha = stored["repository"]["base_sha"]
        .as_str()
        .unwrap()
        .to_string();
    let head_sha = stored["repository"]["head_sha"]
        .as_str()
        .unwrap()
        .to_string();

    let made = scripted(&world, ReviewStatus::Complete, 0);
    let listed = drive(
        made.server,
        &[
            ("gitcko_review_status", json!({})),
            ("gitcko_review_status", json!({"review_id": "not-an-id"})),
            (
                "gitcko_review_status",
                json!({"review_id": ReviewId::new().to_string()}),
            ),
            (
                "gitcko_list_findings",
                json!({"review_id": review_id, "limit": 1}),
            ),
        ],
    )
    .await;
    let status = value_of(listed[0].as_ref().unwrap());
    assert_eq!(status["review_id"], review_id);
    assert_eq!(status["lifecycle"], "completed");
    assert_eq!(status["score_available"], true);
    assert_eq!(status["findings_total"], 2);
    assert_eq!(status["has_new_commits_since_review"], false);
    assert_eq!(status["verification"], "not_run");
    assert_eq!(listed[1].as_ref().unwrap().is_error, Some(true));
    assert_eq!(
        value_of(listed[1].as_ref().unwrap())["code"],
        "INVALID_REQUEST"
    );
    assert_eq!(
        value_of(listed[2].as_ref().unwrap())["code"],
        "REVIEW_NOT_FOUND"
    );

    let page = value_of(listed[3].as_ref().unwrap());
    assert_eq!(page["findings"].as_array().unwrap().len(), 1);
    assert_eq!(page["limit"], 1);
    assert_eq!(page["coverage"]["truncated"], true);
    let next = page["coverage"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(page["findings"][0]["addressed"], false);
    assert_eq!(page["findings"][0]["fix_package"], Value::Null);

    let made = scripted(&world, ReviewStatus::Complete, 0);
    let more = drive(
        made.server,
        &[
            (
                "gitcko_list_findings",
                json!({"review_id": review_id, "limit": 1, "cursor": next}),
            ),
            (
                "gitcko_list_findings",
                json!({"review_id": review_id, "limit": 50, "cursor": next}),
            ),
            (
                "gitcko_list_findings",
                json!({"review_id": review_id, "severity": ["P0"]}),
            ),
            (
                "gitcko_list_findings",
                json!({"review_id": review_id, "severity": ["P2"]}),
            ),
            (
                "gitcko_list_findings",
                json!({"review_id": review_id, "has_suggestion": false}),
            ),
        ],
    )
    .await;
    assert_eq!(
        value_of(more[0].as_ref().unwrap())["findings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        value_of(more[1].as_ref().unwrap())["code"],
        "INVALID_REQUEST"
    );
    assert_eq!(
        value_of(more[2].as_ref().unwrap())["findings"][0]["severity"],
        "P0"
    );
    assert_eq!(
        value_of(more[3].as_ref().unwrap())["findings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        value_of(more[3].as_ref().unwrap())["findings"][0]["severity"],
        "P2"
    );
    assert_eq!(
        value_of(more[4].as_ref().unwrap())["findings"][0]["severity"],
        "P0"
    );

    let repo_key = gitcko_mcp::root_id(world.repo.path()).unwrap();
    let store = ReviewStore::new(&world.cache);
    let mut partial = sample();
    partial.review_id = later_than(&review_id);
    partial.status = ReviewStatus::Partial;
    partial.score = None;
    partial.reason_codes = vec!["budget_exhausted".into()];
    partial.findings.clear();
    partial.target.base_sha = base_sha.clone();
    partial.target.head_sha = head_sha.clone();
    store.put(&repo_key, &partial).unwrap();

    let made = scripted(&world, ReviewStatus::Complete, 0);
    let still = drive(
        made.server,
        &[(
            "gitcko_list_findings",
            json!({"review_id": review_id, "addressed": true}),
        )],
    )
    .await;
    assert!(
        value_of(still[0].as_ref().unwrap())["findings"]
            .as_array()
            .unwrap()
            .is_empty(),
        "a later partial review does not address a finding"
    );

    let mut other_base = sample();
    other_base.review_id = later_than(&partial.review_id);
    other_base.status = ReviewStatus::Complete;
    other_base.findings.clear();
    other_base.target.base_sha = "f".repeat(40);
    other_base.target.head_sha = head_sha.clone();
    store.put(&repo_key, &other_base).unwrap();

    let mut resolved = sample();
    resolved.review_id = later_than(&other_base.review_id);
    resolved.status = ReviewStatus::Complete;
    resolved.findings.clear();
    resolved.target.base_sha = base_sha;
    resolved.target.head_sha = head_sha.clone();
    store.put(&repo_key, &resolved).unwrap();
    std::fs::write(
        world.repo.path().join("src/lib.rs"),
        b"pub fn kept() { let _ = 2; }\n",
    )
    .unwrap();
    commit(world.repo.path(), "after the review");

    let made = scripted(&world, ReviewStatus::Complete, 0);
    let after = drive(
        made.server,
        &[
            (
                "gitcko_list_findings",
                json!({"review_id": review_id, "addressed": true}),
            ),
            (
                "gitcko_review_status",
                json!({"review_id": review_id, "head": "HEAD"}),
            ),
            (
                "gitcko_review_status",
                json!({"review_id": review_id, "head": head_sha}),
            ),
        ],
    )
    .await;
    assert_eq!(
        value_of(after[0].as_ref().unwrap())["findings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        value_of(after[0].as_ref().unwrap())["findings"][0]["addressed"],
        true
    );
    assert_eq!(
        value_of(after[1].as_ref().unwrap())["code"],
        "INVALID_REQUEST"
    );
    let agreed = value_of(after[2].as_ref().unwrap());
    assert_eq!(agreed["review_id"], review_id);
    assert_eq!(agreed["has_new_commits_since_review"], true);

    let running_id = later_than(&resolved.review_id);
    store
        .mark_running(
            &repo_key,
            &running_id,
            &RunningMarker {
                pid: std::process::id(),
                started: "2026-01-01T00:00:00Z".into(),
                head_sha: Some(head_sha.clone()),
                base_sha: Some(stored["repository"]["base_sha"].as_str().unwrap().into()),
            },
        )
        .unwrap();
    let made = scripted(&world, ReviewStatus::Complete, 0);
    let live = drive(
        made.server,
        &[("gitcko_review_status", json!({"review_id": running_id}))],
    )
    .await;
    let live = value_of(live[0].as_ref().unwrap());
    assert_eq!(live["status"], "partial");
    assert_eq!(live["lifecycle"], "investigating");
    assert_eq!(live["reason_codes"], json!(["in_progress"]));
    let _ = stderr;
}

fn later_than(existing: &str) -> String {
    loop {
        let id = ReviewId::new().to_string();
        if id.as_str() > existing {
            return id;
        }
    }
}
