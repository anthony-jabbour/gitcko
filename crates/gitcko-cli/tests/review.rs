// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko review`: exit codes, output, and the checks that happen before a model runs.
//! A fake runner stands in for the engine. Nothing here opens a socket.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gitcko_cli::{Io, ReviewJob, ReviewRunner, RunEnv, RunError, execute_at, repo_cache_id};
use gitcko_protocol::event::{RunEvent, RunEventKind};
use gitcko_protocol::review::{Lifecycle, Review, ReviewStatus, Severity};
use gitcko_redact::canaries;
use gitcko_review::Outcome;
use gitcko_test_support::{TempDir, empty_repo, git, workspace_root};

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
    env: BTreeMap<String, String>,
    cache: PathBuf,
}

fn prepare(config: &str) -> World {
    let repo = empty_repo("review-cli");
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

    let home = TempDir::new("review-home");
    let home_path = home.path().canonicalize().expect("temp home");
    let cache = home_path.join("cache");
    std::fs::create_dir(&cache).unwrap();
    let config_path = home_path.join("config.toml");
    std::fs::write(&config_path, config).unwrap();
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
    World {
        repo,
        _home: home,
        env,
        cache,
    }
}

fn commit(dir: &Path, message: &str) {
    assert!(git(dir).args(["add", "-A"]).status().unwrap().success());
    let status = git(dir)
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
        .unwrap();
    assert!(status.success(), "{status}");
}

struct Seen {
    local_only: bool,
    only_paths: Vec<String>,
    include_paths: Vec<String>,
    review_id: String,
}

struct Fake {
    status: ReviewStatus,
    with_finding: bool,
    resolved: bool,
    seen: Mutex<Option<Seen>>,
}

impl Fake {
    fn status(status: ReviewStatus) -> Fake {
        Fake {
            status,
            with_finding: false,
            resolved: false,
            seen: Mutex::new(None),
        }
    }

    fn with_finding(status: ReviewStatus) -> Fake {
        Fake {
            status,
            with_finding: true,
            resolved: false,
            seen: Mutex::new(None),
        }
    }
}

impl ReviewRunner for Fake {
    fn run(&self, job: &ReviewJob<'_>, on_event: &dyn Fn(&RunEvent)) -> Result<Outcome, RunError> {
        on_event(&RunEvent {
            run_id: job.review_id.clone(),
            seq: 1,
            at: "2026-01-01T00:00:00Z".into(),
            kind: RunEventKind::StepStarted {
                step: "snapshot".into(),
            },
        });
        let mut review = sample();
        review.review_id = job.review_id.clone();
        review.status = self.status;
        if self.status != ReviewStatus::Complete {
            review.score = None;
            review.reason_codes = vec!["budget_exhausted".into()];
        }
        if !self.with_finding {
            review.findings.clear();
        } else if self.resolved {
            review.findings[0].lifecycle = Lifecycle::Resolved;
        } else {
            review.findings[0].severity = Severity::P2;
        }
        *self.seen.lock().unwrap() = Some(Seen {
            local_only: job.local_only,
            only_paths: job
                .only_paths
                .iter()
                .map(|p| p.as_str().to_string())
                .collect(),
            include_paths: job
                .include_paths
                .iter()
                .map(|p| p.as_str().to_string())
                .collect(),
            review_id: job.review_id.clone(),
        });
        Ok(Outcome {
            review,
            dropped: Vec::new(),
            records: Vec::new(),
            events: Vec::new(),
        })
    }
}

fn sample() -> Review {
    let path = workspace_root().join("crates/gitcko-protocol/tests/data/review-full.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn go(
    world: &World,
    args: &[&str],
    runner: Option<&dyn ReviewRunner>,
    tty: bool,
) -> (u8, String, String) {
    let io = Io::capture();
    let code = execute_at(
        args,
        &RunEnv {
            cwd: world.repo.path(),
            env: &world.env,
            runner,
            stdout_is_tty: tty,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
            exe: None,
        },
        &io,
    );
    (code, io.stdout_text(), io.stderr_text())
}

fn script(lines: &[&str]) -> Mutex<VecDeque<String>> {
    Mutex::new(lines.iter().copied().map(str::to_string).collect())
}

fn go_prompt(
    world: &World,
    extra: &[&str],
    runner: &Fake,
    stdin_is_tty: bool,
    answers: Option<&Mutex<VecDeque<String>>>,
            exe: None,
) -> (u8, String, String) {
    let owned = review_args(extra);
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    let io = Io::capture();
    let code = execute_at(
        &refs,
        &RunEnv {
            cwd: world.repo.path(),
            env: &world.env,
            runner: Some(runner),
            stdout_is_tty: false,
            stdin_is_tty,
            tui: None,
            keychain: None,
            answers,
            exe: None,
        },
        &io,
    );
    (code, io.stdout_text(), io.stderr_text())
}

fn review_args(extra: &[&str]) -> Vec<String> {
    let mut args = vec!["review".into(), "--base".into(), "base".into()];
    args.extend(extra.iter().map(|s| (*s).to_string()));
    args
}

fn go_review(world: &World, extra: &[&str], runner: &Fake) -> (u8, String, String) {
    let owned = review_args(extra);
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    go(world, &refs, Some(runner), false)
}

#[test]
fn exit_codes_follow_status_then_fail_on() {
    let world = prepare(LOCAL);
    let clean = Fake::status(ReviewStatus::Complete);
    let (code, out, err) = go_review(&world, &[], &clean);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("No issues found"), "{out}");
    assert!(out.starts_with("status: complete"), "{out}");
    assert!(err.contains("stage: snapshot"), "{err}");
    assert!(err.contains("local: code stays on this machine"), "{err}");
    assert!(clean.seen.lock().unwrap().as_ref().unwrap().local_only);

    let hit = Fake::with_finding(ReviewStatus::Complete);
    let (code, out, _) = go_review(&world, &["--fail-on", "P2"], &hit);
    assert_eq!(code, 7, "{out}");
    assert!(!out.contains("No issues found"), "{out}");

    let below = Fake::with_finding(ReviewStatus::Complete);
    let (code, _, _) = go_review(&world, &["--fail-on", "P0"], &below);
    assert_eq!(code, 0, "a P2 finding does not trip --fail-on P0");

    let partial = Fake::with_finding(ReviewStatus::Partial);
    let (code, out, err) = go_review(&world, &["--fail-on", "P0"], &partial);
    assert_eq!(code, 1, "a partial review is not a findings failure");
    assert!(out.starts_with("status: partial"), "{out}");
    assert!(!out.contains("No issues found"), "{out}");
    assert!(err.contains("status: partial"), "{err}");

    let failed = Fake::status(ReviewStatus::Failed);
    assert_eq!(go_review(&world, &[], &failed).0, 1);
    let canceled = Fake::with_finding(ReviewStatus::Canceled);
    assert_eq!(go_review(&world, &["--fail-on", "P0"], &canceled).0, 130);
}

#[test]
fn json_matches_the_review_schema_and_a_partial_does_not_look_clean() {
    let world = prepare(LOCAL);
    let partial = Fake::status(ReviewStatus::Partial);
    let (code, out, err) = go_review(&world, &["--json"], &partial);
    assert_eq!(code, 1, "{err}");
    assert!(!out.contains("No issues found"), "{out}");
    assert!(err.contains("status: partial"), "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(value["status"], "partial");
    assert!(value["score"].is_null());
    let schema_path = workspace_root().join("integrations/schemas/review-1.json");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(schema_path).unwrap()).unwrap();
    jsonschema::validate(&schema, &value).expect("partial review is gitcko.review/1");

    let clean = Fake::status(ReviewStatus::Complete);
    let (code, out, _) = go_review(&world, &["--json"], &clean);
    assert_eq!(code, 0);
    let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    jsonschema::validate(&schema, &value).expect("complete review is gitcko.review/1");
    assert_eq!(value["status"], "complete");
}

#[test]
fn unavailable_flags_exit_6_after_a_bad_path_exits_2() {
    let world = prepare(LOCAL);
    let runner = Fake::status(ReviewStatus::Complete);
    let (code, _, err) = go_review(&world, &["--plus"], &runner);
    assert_eq!(code, 6, "{err}");
    assert!(
        err.contains("notice: --plus is not available in this build"),
        "{err}"
    );
    assert!(runner.seen.lock().unwrap().is_none());

    let (code, _, err) = go_review(&world, &["--layout", "diff"], &runner);
    assert_eq!(code, 6, "{err}");
    assert!(err.contains("notice: --layout diff"), "{err}");

    let (code, _, err) = go_review(&world, &["--plus", "--paths", "../outside"], &runner);
    assert_eq!(code, 2, "{err}");
    assert!(!err.contains("notice:"), "{err}");
    assert!(
        !err.contains("outside"),
        "the rejected path is not echoed: {err}"
    );
}

#[test]
fn a_missing_base_and_repository_models_stop_before_the_runner() {
    let world = prepare(LOCAL);
    let runner = Fake::status(ReviewStatus::Complete);
    let (code, _, err) = go(
        &world,
        &["review", "--base", "no-such-ref"],
        Some(&runner),
        false,
    );
    assert_eq!(code, 2, "{err}");
    assert!(!err.contains("no-such-ref"), "{err}");
    assert!(runner.seen.lock().unwrap().is_none());

    let repo_models = r#"
schema_version = 1
[models.bindings.smart]
provider = "ollama"
model = "fixture:1"
endpoint = "http://127.0.0.1:11434"
"#;
    let user_models = r#"
[models.bindings.smart]
provider = "ollama"
model = "fixture:1"
endpoint = "http://127.0.0.1:11434"
"#;
    std::fs::create_dir(world.repo.path().join(".gitcko")).unwrap();
    std::fs::write(world.repo.path().join(".gitcko/config.toml"), repo_models).unwrap();
    let (code, _, err) = go_review(&world, &[], &runner);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("models"), "{err}");
    assert!(
        runner.seen.lock().unwrap().is_none(),
        "repo models never start a run"
    );

    let user_home = prepare(user_models);
    let runner = Fake::status(ReviewStatus::Complete);
    let (code, _, err) = go_review(&user_home, &[], &runner);
    assert_eq!(code, 0, "{err}");
    assert!(
        runner.seen.lock().unwrap().is_some(),
        "the same table in the user file runs"
    );
}

#[test]
fn an_unacknowledged_remote_never_calls_the_runner_and_an_acknowledged_one_does() {
    let remote = r#"
[models.bindings.smart]
provider = "openrouter"
model = "example/model-1"
"#;
    let mut closed = prepare(remote);
    closed.env.insert(
        "OPENROUTER_API_KEY".into(),
        format!("{}{}", "not-a-", "key"),
    );
    let runner = Fake::status(ReviewStatus::Complete);
    let (code, _, err) = go_review(&closed, &["--no-input"], &runner);
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("gitcko setup --accept-remote openrouter"),
        "{err}"
    );
    assert!(runner.seen.lock().unwrap().is_none());

    let open = format!("{remote}\n[privacy]\nremote_acknowledged = [\"openrouter\"]\n");
    let mut opened = prepare(&open);
    opened.env.insert(
        "OPENROUTER_API_KEY".into(),
        format!("{}{}", "not-a-", "key"),
    );
    let runner = Fake::status(ReviewStatus::Complete);
    let (code, _, err) = go_review(&opened, &[], &runner);
    assert_eq!(code, 0, "{err}");
    let seen = runner.seen.lock().unwrap();
    let seen = seen
        .as_ref()
        .expect("an acknowledged remote reaches the runner");
    assert!(!seen.local_only);
    assert!(
        err.contains("remote (Review): selected code is sent to openrouter"),
        "{err}"
    );
    assert!(
        err.contains("remote (Judge): selected code is sent to openrouter"),
        "judge bindings are disclosed too: {err}"
    );
}

#[test]
fn paths_and_include_reach_the_runner_and_the_review_is_stored() {
    let world = prepare(LOCAL);
    let runner = Fake::status(ReviewStatus::Complete);
    let (code, _, err) = go_review(
        &world,
        &["--paths", "src/lib.rs", "--include", ".env"],
        &runner,
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("include: .env"), "{err}");
    let seen = runner.seen.lock().unwrap();
    let seen = seen.as_ref().unwrap();
    assert_eq!(seen.only_paths, ["src/lib.rs"]);
    assert_eq!(seen.include_paths, [".env"]);

    let mut files = Vec::new();
    let repos = world.cache.join("repos");
    for repo in std::fs::read_dir(&repos).unwrap() {
        for file in std::fs::read_dir(repo.unwrap().path().join("reviews")).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }
    }
    assert_eq!(files.len(), 1, "{files:?}");
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&files[0]).unwrap()).unwrap();
    assert_eq!(stored["review_id"], seen.review_id);
    assert!(
        std::fs::read_dir(files[0].parent().unwrap())
            .unwrap()
            .all(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_none_or(|ext| ext != "running")),
        "a finished review does not stay marked running"
    );
}

#[test]
fn a_terminal_gets_a_box_and_agent_stays_plain_text() {
    let world = prepare(LOCAL);
    let boxed = Fake::status(ReviewStatus::Complete);
    let owned = review_args(&[]);
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (code, out, _) = go(&world, &refs, Some(&boxed), true);
    assert_eq!(code, 0);
    assert!(out.starts_with('+'), "{out}");

    let plain = Fake::status(ReviewStatus::Complete);
    let (code, out, _) = go_review(&world, &["--agent"], &plain);
    assert_eq!(code, 0);
    assert!(out.starts_with("status: complete"), "{out}");
    assert!(!out.starts_with('+'), "{out}");
}

#[test]
fn no_models_exits_2_and_names_setup() {
    let world = prepare("# no models\n");
    let runner = Fake::status(ReviewStatus::Complete);
    let (code, _, err) = go_review(&world, &[], &runner);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("gitcko setup"), "{err}");
    assert!(runner.seen.lock().unwrap().is_none());
}

struct BrokenStdout;

impl Write for BrokenStdout {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("broken stdout"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_broken_stdout_during_present_is_exit_1() {
    let world = prepare(LOCAL);
    let runner = Fake::status(ReviewStatus::Complete);
    let owned = review_args(&["--json"]);
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    let io = Io {
        stdout: Arc::new(Mutex::new(BrokenStdout)),
        stderr: Arc::new(Mutex::new(Vec::new())),
    };
    let code = execute_at(
        &refs,
        &RunEnv {
            cwd: world.repo.path(),
            env: &world.env,
            runner: Some(&runner),
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
            exe: None,
        },
        &io,
    );
    assert_eq!(
        code,
        1,
        "{}",
        String::from_utf8_lossy(&io.stderr.lock().unwrap())
    );
}

#[cfg(debug_assertions)]
#[test]
fn store_put_failure_clears_the_marker_and_exits_1() {
    let mut world = prepare(LOCAL);
    world.env.insert("GITCKO_TEST_PUT_FAIL".into(), "1".into());
    let repo_id = repo_cache_id(world.repo.path()).unwrap();
    let reviews = world.cache.join("repos").join(repo_id).join("reviews");
    let runner = Fake::status(ReviewStatus::Complete);
    let owned = review_args(&["--json"]);
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    let io = Io::capture();
    let code = execute_at(
        &refs,
        &RunEnv {
            cwd: world.repo.path(),
            env: &world.env,
            runner: Some(&runner),
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
            exe: None,
        },
        &io,
    );
    assert_eq!(code, 1, "{}", io.stderr_text());
    assert!(
        io.stderr_text().contains("could not store the review"),
        "{}",
        io.stderr_text()
    );
    assert!(
        io.stdout_text().contains("\"schema_version\""),
        "present the review even when storage fails: {}",
        io.stdout_text()
    );
    let review_id = runner
        .seen
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .review_id
        .clone();
    assert!(
        !reviews.join(format!("{review_id}.running")).exists(),
        "marker cleared after a failed put"
    );
}

#[cfg(all(unix, debug_assertions))]
#[test]
fn the_binary_turns_ctrl_c_into_130() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let mut child = Command::new(env!("CARGO_BIN_EXE_gitcko"));
    child
        .arg("review")
        .env_clear()
        .env("GITCKO_TEST_SEAM", "1")
        .env("GITCKO_TEST_WAIT_CANCEL", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Ok(path) = std::env::var("PATH") {
        child.env("PATH", path);
    }
    let mut child = child.spawn().expect("spawn gitcko");
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(stderr).lines();
        while let Some(Ok(line)) = lines.next() {
            if line.contains("waiting for cancel") {
                let _ = tx.send(());
                break;
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the binary should announce it is waiting");
    let status = Command::new("/bin/kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill");
    assert!(status.success());
    let finished = child.wait().expect("wait");
    // A raw SIGINT kill (handler not yet installed) yields `code() == None`. The
    // product must convert Ctrl-C into exit 130 instead.
    assert_eq!(
        finished.code(),
        Some(130),
        "expected exit 130, got {finished:?} (signal kill means the handler raced)"
    );
}

#[test]
fn a_headless_remote_does_not_call_and_an_interactive_yes_does() {
    let remote = r#"
color = true
[models.bindings.smart]
provider = "openrouter"
model = "example/model-1"
"#;
    let key = canaries()[3].clone();
    let mut world = prepare(remote);
    world.env.insert("OPENROUTER_API_KEY".into(), key.clone());
    let config = world.env.get("GITCKO_CONFIG").expect("config path").clone();

    let runner = Fake::status(ReviewStatus::Complete);
    let yes = script(&["y"]);
    let (code, out, err) = go_prompt(&world, &["--no-input", "--json"], &runner, true, Some(&yes));
    assert_eq!(code, 2, "{err}{out}");
    assert!(runner.seen.lock().unwrap().is_none());
    assert!(!err.contains("selected code is sent to"), "{err}");
    assert!(!out.contains("remote:"), "{out}");
    assert!(!err.contains(&key) && !out.contains(&key));
    assert!(
        !std::fs::read_to_string(&config)
            .unwrap()
            .contains("remote_acknowledged")
    );

    world.env.insert("CI".into(), "1".into());
    let runner = Fake::status(ReviewStatus::Complete);
    let yes = script(&["y"]);
    let (code, out, err) = go_prompt(&world, &["--json"], &runner, true, Some(&yes));
    assert_eq!(code, 2, "{err}");
    assert!(runner.seen.lock().unwrap().is_none());
    assert!(
        !err.contains("selected code is sent to") && !out.contains(&key) && !err.contains(&key)
    );
    world.env.remove("CI");

    let runner = Fake::status(ReviewStatus::Complete);
    let no = script(&["n"]);
    let (code, out, err) = go_prompt(&world, &["--json"], &runner, true, Some(&no));
    assert_eq!(code, 2, "{err}");
    assert!(runner.seen.lock().unwrap().is_none());
    assert!(err.contains("Acknowledge"), "{err}");
    assert!(!err.contains("selected code is sent to"), "{err}");
    assert!(!out.contains("remote:") && !out.contains(&key));

    let runner = Fake::status(ReviewStatus::Complete);
    let blank = script(&[""]);
    let (code, _, err) = go_prompt(&world, &[], &runner, true, Some(&blank));
    assert_eq!(code, 2, "{err}");
    assert!(runner.seen.lock().unwrap().is_none());
    assert!(!err.contains("selected code is sent to"), "{err}");

    let runner = Fake::status(ReviewStatus::Complete);
    let yes = script(&["yes"]);
    let (code, out, err) = go_prompt(&world, &["--json"], &runner, true, Some(&yes));
    assert_eq!(code, 0, "{err}{out}");
    let seen = runner.seen.lock().unwrap();
    assert!(!seen.as_ref().expect("yes reaches the runner").local_only);
    assert!(!out.contains("remote:"), "{out}");
    assert!(!out.contains(&key) && !err.contains(&key));
    let remote_at = err.find("selected code is sent to").expect("disclosure");
    let stage_at = err.find("stage:").expect("progress");
    assert!(remote_at < stage_at, "{err}");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("remote_acknowledged"), "{text}");
    assert!(text.contains("example/model-1"), "{text}");
    assert!(text.contains("true"), "{text}");
    assert!(!text.contains(&key));

    let ollama = r#"
[models.bindings.smart]
provider = "ollama"
model = "fixture:1"
"#;
    let mut remote_ollama = prepare(ollama);
    remote_ollama.env.insert(
        "OLLAMA_HOST".into(),
        "https://ollama.example.test:11434".into(),
    );
    let runner = Fake::status(ReviewStatus::Complete);
    let yes = script(&["y"]);
    let (code, out, err) = go_prompt(&remote_ollama, &[], &runner, true, Some(&yes));
    assert_eq!(code, 2, "{err}{out}");
    assert!(runner.seen.lock().unwrap().is_none());
    assert!(
        !err.contains("example.test") && !out.contains("example.test"),
        "{err}"
    );
    assert!(!err.contains("selected code is sent to"), "{err}");
}
