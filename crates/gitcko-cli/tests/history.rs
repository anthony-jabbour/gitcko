// SPDX-License-Identifier: AGPL-3.0-or-later

//! `review show` and `review status` read the store. No model is constructed.

use std::collections::BTreeMap;

use gitcko_cli::{Io, RunEnv, execute_at, repo_cache_id};
use gitcko_protocol::event::{RunEvent, RunEventKind};
use gitcko_protocol::review::{Lifecycle, Review, ReviewStatus, Severity};
use gitcko_storage::{ReviewStore, RunningMarker};
use gitcko_test_support::{TempDir, empty_repo, git, workspace_root};

struct World {
    repo: TempDir,
    _home: TempDir,
    env: BTreeMap<String, String>,
    store: ReviewStore,
    head: String,
}

fn world() -> World {
    let repo = empty_repo("history");
    std::fs::write(repo.path().join("README"), b"hello\n").unwrap();
    assert!(
        git(repo.path())
            .args(["add", "README"])
            .status()
            .unwrap()
            .success()
    );
    let status = git(repo.path())
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@fixture.invalid",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let head = git(repo.path())
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let head = String::from_utf8(head.stdout).unwrap().trim().to_string();

    let home = TempDir::new("history-home");
    let home_path = home.path().canonicalize().unwrap();
    let cache = home_path.join("cache");
    std::fs::create_dir(&cache).unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), home_path.display().to_string()),
        ("APPDATA".into(), home_path.display().to_string()),
        (
            "LOCALAPPDATA".into(),
            home_path.join("local").display().to_string(),
        ),
        (
            "GITCKO_CONFIG".into(),
            home_path.join("config.toml").display().to_string(),
        ),
        ("GITCKO_CACHE_DIR".into(), cache.display().to_string()),
    ]);
    let store = ReviewStore::new(&cache);
    World {
        repo,
        _home: home,
        env,
        store,
        head,
    }
}

fn sample() -> Review {
    let path = workspace_root().join("crates/gitcko-protocol/tests/data/review-full.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn put(world: &World, id: &str, status: ReviewStatus, head: &str) -> Review {
    let mut review = sample();
    review.review_id = id.into();
    review.status = status;
    if status != ReviewStatus::Complete {
        review.score = None;
        review.reason_codes = vec!["budget_exhausted".into()];
    }
    review.target.head_sha = head.into();
    review.target.base_sha = world.head.clone();
    world
        .store
        .put(&repo_cache_id(world.repo.path()).unwrap(), &review)
        .unwrap();
    review
}

fn go(world: &World, args: &[&str]) -> (u8, String, String) {
    let io = Io::capture();
    let code = execute_at(
        args,
        &RunEnv {
            cwd: world.repo.path(),
            env: &world.env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            exe: None,
        },
        &io,
    );
    (code, io.stdout_text(), io.stderr_text())
}

#[test]
fn status_codes_follow_the_stored_review_and_a_missing_one_is_1() {
    let world = world();
    let (code, _, err) = go(&world, &["review", "status"]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("no review"), "{err}");

    put(
        &world,
        "rv_01AAAAAAAAAAAAAA0000000000",
        ReviewStatus::Complete,
        &world.head,
    );
    let (code, out, err) = go(&world, &["review", "status"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("status: complete"), "{out}");

    put(
        &world,
        "rv_01BBBBBBBBBBBBBB0000000000",
        ReviewStatus::Partial,
        &world.head,
    );
    let (code, out, err) = go(&world, &["review", "status", "--json"]);
    assert_eq!(code, 1, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(value["status"], "partial");
    assert_eq!(value["lifecycle"], "partial");
    assert!(value["completed_at"].is_null());
    assert_eq!(value["schema_version"], "gitcko.review/1");
    assert_eq!(value["reason_codes"][0], "budget_exhausted");
    assert_eq!(value["head_sha"], world.head);

    put(
        &world,
        "rv_01CCCCCCCCCCCCCC0000000000",
        ReviewStatus::Failed,
        &world.head,
    );
    assert_eq!(go(&world, &["review", "status"]).0, 4);
    put(
        &world,
        "rv_01DDDDDDDDDDDDDD0000000000",
        ReviewStatus::Canceled,
        &world.head,
    );
    assert_eq!(go(&world, &["review", "status"]).0, 5);
}

#[test]
fn a_live_marker_is_running_and_a_dead_one_without_json_is_failed() {
    let world = world();
    let id = "rv_01AAAAAAAAAAAAAA0000000000";
    put(&world, id, ReviewStatus::Complete, &world.head);
    let repo_id = repo_cache_id(world.repo.path()).unwrap();
    world
        .store
        .mark_running(
            &repo_id,
            id,
            &RunningMarker {
                pid: std::process::id(),
                started: "2026-01-01T00:00:00Z".into(),
                head_sha: Some(world.head.clone()),
                base_sha: Some(world.head.clone()),
            },
        )
        .unwrap();
    let (code, out, _) = go(&world, &["review", "status"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("status: running"), "{out}");

    world.store.clear_running(&repo_id, id).unwrap();
    let (code, _, _) = go(&world, &["review", "status"]);
    assert_eq!(code, 0, "a stored complete review is not a failure");

    world
        .store
        .mark_running(
            &repo_id,
            "rv_01ZZZZZZZZZZZZZZ0000000000",
            &RunningMarker {
                pid: u32::MAX,
                started: "2026-01-01T00:00:00Z".into(),
                head_sha: Some(world.head.clone()),
                base_sha: None,
            },
        )
        .unwrap();
    assert_eq!(
        go(&world, &["review", "status"]).0,
        4,
        "a newer marker whose process is gone is a failed run"
    );
}

#[test]
fn show_json_is_the_stored_bytes_and_partial_text_is_not_clean() {
    let world = world();
    let id = "rv_01AAAAAAAAAAAAAA0000000000";
    put(&world, id, ReviewStatus::Partial, &world.head);
    let (code, out, err) = go(&world, &["review", "show", id, "--text"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("status: partial"), "{out}");
    assert!(!out.contains("No issues found"), "{out}");

    put(
        &world,
        "rv_01BBBBBBBBBBBBBB0000000000",
        ReviewStatus::Complete,
        &world.head,
    );
    let mut clean = sample();
    clean.review_id = "rv_01CCCCCCCCCCCCCC0000000000".into();
    clean.findings.clear();
    clean.target.head_sha = world.head.clone();
    world
        .store
        .put(&repo_cache_id(world.repo.path()).unwrap(), &clean)
        .unwrap();
    let (code, out, _) = go(&world, &["review", "show", &clean.review_id, "--text"]);
    assert_eq!(code, 0);
    assert!(out.contains("No issues found"), "{out}");

    let (code, out, err) = go(&world, &["review", "show", id, "--json"]);
    assert_eq!(code, 0, "{err}");
    let path = world
        .store
        .root()
        .join("repos")
        .join(repo_cache_id(world.repo.path()).unwrap())
        .join("reviews")
        .join(format!("{id}.json"));
    let stored = std::fs::read(&path).unwrap();
    assert_eq!(out.into_bytes(), stored);

    let (missing, _, err) = go(&world, &["review", "show", "rv_01EEEEEEEEEEEEEE0000000000"]);
    assert_eq!(missing, 2, "{err}");
    assert!(err.contains("no such review"), "{err}");
}

#[test]
fn the_list_names_the_branch_only_for_the_current_head_and_counts_open_findings() {
    let world = world();
    let mut older = sample();
    older.review_id = "rv_01AAAAAAAAAAAAAA0000000000".into();
    older.target.head_sha = world.head.clone();
    older.findings[0].severity = Severity::P2;
    let mut resolved = older.findings[0].clone();
    resolved.severity = Severity::P0;
    resolved.lifecycle = Lifecycle::Resolved;
    let mut open = older.findings[0].clone();
    open.severity = Severity::P0;
    older.findings.push(resolved);
    older.findings.push(open);
    world
        .store
        .put(&repo_cache_id(world.repo.path()).unwrap(), &older)
        .unwrap();

    let mut newer = sample();
    newer.review_id = "rv_01ZZZZZZZZZZZZZZ0000000000".into();
    newer.target.head_sha = "0123456789abcdef0123456789abcdef01234567".into();
    newer.findings.clear();
    world
        .store
        .put(&repo_cache_id(world.repo.path()).unwrap(), &newer)
        .unwrap();

    let (code, out, err) = go(&world, &["review", "show", "--json"]);
    assert_eq!(code, 0, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(value["schema_version"], "gitcko.review-list/1");
    assert_eq!(value["reviews"][0]["review_id"], newer.review_id);
    assert!(value["reviews"][0]["branch"].is_null(), "{value}");
    assert_eq!(value["reviews"][1]["branch"], "main");
    assert_eq!(value["reviews"][1]["counts"]["P0"], 1);
    assert_eq!(value["reviews"][1]["counts"]["P2"], 1);
    assert!(value["reviews"][1]["completed_at"].is_null());
}

#[test]
fn show_events_follow_the_review_and_a_bad_commit_does_not_echo_the_ref() {
    let world = world();
    let id = "rv_01AAAAAAAAAAAAAA0000000000";
    put(&world, id, ReviewStatus::Complete, &world.head);
    let event = RunEvent {
        run_id: id.into(),
        seq: 1,
        at: "2026-01-01T00:00:00Z".into(),
        kind: RunEventKind::StepStarted {
            step: "snapshot".into(),
        },
    };
    let path = world
        .store
        .root()
        .join("repos")
        .join(repo_cache_id(world.repo.path()).unwrap())
        .join("reviews")
        .join(format!("{id}.events"));
    std::fs::write(&path, serde_json::to_vec(&vec![event]).unwrap()).unwrap();
    let (code, out, _) = go(&world, &["review", "show", id, "--events", "--text"]);
    assert_eq!(code, 0);
    assert!(out.contains("step.started"), "{out}");

    let (code, _, err) = go(&world, &["review", "status", "--commit", "no-such-ref"]);
    assert_eq!(code, 2, "{err}");
    assert!(!err.contains("no-such-ref"), "{err}");
}
