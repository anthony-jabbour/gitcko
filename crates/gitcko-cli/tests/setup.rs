// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko setup`: split bindings, remote acknowledgement, and a key that is stored
//! but never printed.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gitcko_cli::{Io, ReviewJob, ReviewRunner, RunEnv, RunError, execute_at};
use gitcko_config::{CredError, GitckoPaths, Keychain};
use gitcko_model::resolve;
use gitcko_protocol::config::{ModelRole, UserConfig};
use gitcko_protocol::event::{RunEvent, RunEventKind};
use gitcko_protocol::review::{Review, ReviewStatus};
use gitcko_redact::canaries;
use gitcko_review::Outcome;
use gitcko_test_support::{TempDir, empty_repo, git, workspace_root};

struct World {
    repo: TempDir,
    _home: TempDir,
    env: BTreeMap<String, String>,
    config: PathBuf,
}

fn home(user: &str) -> World {
    let repo = empty_repo("setup-cli");
    let home = TempDir::new("setup-home");
    let home_path = home.path().canonicalize().expect("temp home");
    let config = home_path.join("config.toml");
    if !user.is_empty() {
        std::fs::write(&config, user).unwrap();
    }
    let env = BTreeMap::from([
        ("HOME".into(), home_path.display().to_string()),
        ("APPDATA".into(), home_path.display().to_string()),
        (
            "LOCALAPPDATA".into(),
            home_path.join("local").display().to_string(),
        ),
        ("GITCKO_CONFIG".into(), config.display().to_string()),
        (
            "GITCKO_CACHE_DIR".into(),
            home_path.join("cache").display().to_string(),
        ),
    ]);
    World {
        repo,
        _home: home,
        env,
        config,
    }
}

struct Store {
    fail_set: bool,
    entries: Mutex<BTreeMap<String, String>>,
}

impl Store {
    fn new() -> Store {
        Store {
            fail_set: false,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    fn failing() -> Store {
        Store {
            fail_set: true,
            entries: Mutex::new(BTreeMap::new()),
        }
    }
}

impl Keychain for Store {
    fn get(&self, account: &str) -> Result<Option<String>, CredError> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), CredError> {
        if self.fail_set {
            return Err(CredError::Keychain);
        }
        self.entries
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }
}

fn lines(items: &[&str]) -> Mutex<VecDeque<String>> {
    Mutex::new(items.iter().map(|item| (*item).to_string()).collect())
}

fn go(
    world: &World,
    args: &[&str],
    keychain: Option<&dyn Keychain>,
    tty: bool,
    answers: Option<&Mutex<VecDeque<String>>>,
) -> (u8, String, String) {
    let io = Io::capture();
    let code = execute_at(
        args,
        &RunEnv {
            cwd: world.repo.path(),
            env: &world.env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: tty,
            tui: None,
            keychain,
            answers,
        },
        &io,
    );
    (code, io.stdout_text(), io.stderr_text())
}

fn user_file(world: &World) -> UserConfig {
    let text = std::fs::read_to_string(&world.config).expect("config written");
    toml::from_str(&text).expect("config parses")
}

fn credentials_path(world: &World) -> PathBuf {
    GitckoPaths::from_env(&world.env)
        .expect("paths")
        .credentials_file
}

#[test]
fn help_lists_setup_flags() {
    let world = home("");
    let (code, out, err) = go(&world, &["setup", "--help"], None, false, None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("--provider"), "{out}");
    assert!(out.contains("--model"), "{out}");
    assert!(out.contains("--accept-remote"), "{out}");
    assert!(out.contains("--no-input"), "{out}");
}

#[test]
fn no_input_lists_what_is_missing_and_writes_nothing() {
    let world = home("");
    let (code, out, err) = go(&world, &["setup", "--no-input"], None, true, None);
    assert_eq!(code, 2, "{out}");
    assert!(err.contains("missing --provider"), "{err}");
    assert!(err.contains("missing --model"), "{err}");
    assert!(!world.config.exists());

    let mut env = world.env.clone();
    env.insert("OPENROUTER_API_KEY".into(), "present".into());
    let io = Io::capture();
    let code = execute_at(
        &[
            "setup",
            "--no-input",
            "--provider",
            "openrouter",
            "--model",
            "anthropic/claude-haiku-5.5",
        ],
        &RunEnv {
            cwd: world.repo.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
        },
        &io,
    );
    assert_eq!(code, 2, "{}", io.stdout_text());
    assert!(
        io.stderr_text()
            .contains("missing --accept-remote openrouter"),
        "{}",
        io.stderr_text()
    );
    assert!(!world.config.exists());
}

#[test]
fn ollama_setup_writes_two_bindings_and_keeps_other_settings() {
    let world = home("color = true\n");
    let (code, out, err) = go(
        &world,
        &[
            "setup",
            "--no-input",
            "--provider",
            "ollama",
            "--model",
            "fixture:1",
        ],
        None,
        false,
        None,
    );
    assert_eq!(code, 0, "{err}{out}");
    assert!(err.contains("local: code stays on this machine"), "{err}");
    let user = user_file(&world);
    assert_eq!(user.color, Some(true));
    let models = user.models.expect("models");
    let bindings = models.bindings.clone().expect("bindings");
    assert_eq!(bindings["review"].model, "fixture:1");
    assert_eq!(bindings["risk-classifier"].model, "fixture:1");
    assert!(bindings.contains_key("review") && bindings.contains_key("risk-classifier"));
    let resolved = resolve(&models, &world.env).expect("bindings resolve");
    assert_eq!(resolved.bindings["review"].context, Some(131_072));
    assert_eq!(resolved.bindings["risk-classifier"].context, Some(32_768));
    assert_eq!(resolved.roles[&ModelRole::Review], "review");
    assert_eq!(resolved.roles[&ModelRole::Judge], "review");
    assert_eq!(
        resolved.roles[&ModelRole::RiskClassifier],
        "risk-classifier"
    );
    resolved
        .require(&[ModelRole::Review, ModelRole::Judge])
        .unwrap();
}

#[test]
fn openrouter_setup_records_acknowledgement_and_not_the_key() {
    let world = home("color = true\n");
    let mut env = world.env.clone();
    let key = canaries()[1].clone();
    env.insert("OPENROUTER_API_KEY".into(), key.clone());
    let io = Io::capture();
    let code = execute_at(
        &[
            "setup",
            "--no-input",
            "--provider",
            "openrouter",
            "--model",
            "anthropic/claude-haiku-5.5",
            "--accept-remote",
            "openrouter",
        ],
        &RunEnv {
            cwd: world.repo.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
        },
        &io,
    );
    let err = io.stderr_text();
    let out = io.stdout_text();
    assert_eq!(code, 0, "{err}{out}");
    assert!(
        err.contains("remote: selected code is sent to openrouter (anthropic/claude-haiku-5.5)"),
        "{err}"
    );
    assert!(!err.contains(&key) && !out.contains(&key));
    let text = std::fs::read_to_string(&world.config).unwrap();
    assert!(!text.contains(&key));
    assert!(!credentials_path(&world).exists());
    let user: UserConfig = toml::from_str(&text).unwrap();
    assert_eq!(user.color, Some(true));
    let privacy = user.privacy.expect("privacy");
    let ack = privacy.remote_acknowledged.expect("ack");
    assert_eq!(ack.len(), 1);
    let models = user.models.expect("models");
    let bindings = models.bindings.expect("bindings");
    assert_eq!(bindings["review"].model, "anthropic/claude-haiku-5.5");
    assert_eq!(
        bindings["risk-classifier"].model,
        "anthropic/claude-haiku-5.5"
    );
    assert_eq!(models.roles.expect("roles")[&ModelRole::Judge], "review");
}

#[test]
fn a_missing_openrouter_key_is_named_and_nothing_is_written() {
    let world = home("");
    let (code, _out, err) = go(
        &world,
        &[
            "setup",
            "--no-input",
            "--provider",
            "openrouter",
            "--model",
            "anthropic/claude-haiku-5.5",
            "--accept-remote",
            "openrouter",
        ],
        None,
        false,
        None,
    );
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("missing OPENROUTER_API_KEY"), "{err}");
    assert!(!world.config.exists());
}

#[test]
fn ci_does_not_prompt_when_the_flags_are_complete() {
    let world = home("");
    let mut env = world.env.clone();
    env.insert("CI".into(), "true".into());
    let answers = lines(&[]);
    let io = Io::capture();
    let code = execute_at(
        &["setup", "--provider", "ollama", "--model", "fixture:1"],
        &RunEnv {
            cwd: world.repo.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: true,
            tui: None,
            keychain: None,
            answers: Some(&answers),
        },
        &io,
    );
    assert_eq!(code, 0, "{}", io.stderr_text());
    assert!(world.config.exists());
}

#[test]
fn interactive_ollama_uses_the_answers_and_a_bad_provider_is_not_echoed() {
    let world = home("");
    let canary = canaries()[3].clone();
    let bad = lines(&[&canary]);
    let (code, out, err) = go(&world, &["setup"], None, true, Some(&bad));
    assert_eq!(code, 2, "{out}");
    assert!(err.contains("expected ollama or openrouter"), "{err}");
    assert!(!err.contains(&canary) && !out.contains(&canary));
    assert!(!world.config.exists());

    let good = lines(&["ollama", "fixture:1"]);
    let (code, _out, err) = go(&world, &["setup"], None, true, Some(&good));
    assert_eq!(code, 0, "{err}");
    let user = user_file(&world);
    assert_eq!(
        user.models.unwrap().bindings.unwrap()["review"].model,
        "fixture:1"
    );
}

#[test]
fn canary_an_interactive_key_is_stored_and_never_printed() {
    let world = home("");
    let key = canaries()[5].clone();
    let store = Store::new();
    let answers = lines(&[
        "openrouter",
        "anthropic/claude-haiku-5.5",
        "y",
        key.as_str(),
    ]);
    let (code, out, err) = go(&world, &["setup"], Some(&store), true, Some(&answers));
    assert_eq!(code, 0, "{err}{out}");
    assert!(!err.contains(&key) && !out.contains(&key));
    assert!(
        !std::fs::read_to_string(&world.config)
            .unwrap()
            .contains(&key)
    );
    assert_eq!(
        store.get("openrouter").unwrap().as_deref(),
        Some(key.as_str())
    );
    let user = user_file(&world);
    assert!(user.privacy.unwrap().remote_acknowledged.is_some());
    assert_eq!(
        user.models.unwrap().bindings.unwrap()["risk-classifier"].model,
        "anthropic/claude-haiku-5.5"
    );
    assert!(!credentials_path(&world).exists());
}

#[test]
fn declining_remote_setup_writes_nothing() {
    let world = home("");
    let answers = lines(&["openrouter", "anthropic/claude-haiku-5.5", "n"]);
    let store = Store::new();
    let (code, _out, err) = go(&world, &["setup"], Some(&store), true, Some(&answers));
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("remote setup was not acknowledged"), "{err}");
    assert!(
        err.contains("remote: selected code is sent to openrouter (anthropic/claude-haiku-5.5)"),
        "{err}"
    );
    assert!(!world.config.exists());
    assert!(store.get("openrouter").unwrap().is_none());
}

#[test]
fn a_keychain_miss_falls_back_to_a_private_credentials_file() {
    let world = home("");
    let key = canaries()[1].clone();
    let store = Store::failing();
    let answers = lines(&[
        "openrouter",
        "anthropic/claude-haiku-5.5",
        "yes",
        key.as_str(),
    ]);
    let (code, out, err) = go(&world, &["setup"], Some(&store), true, Some(&answers));
    assert_eq!(code, 0, "{err}{out}");
    assert!(!err.contains(&key) && !out.contains(&key));
    let path = credentials_path(&world);
    let body = std::fs::read_to_string(&path).expect("credentials file");
    assert!(body.contains(&key));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "credentials file mode {mode:o}");
    }
    let (code, out, err) = go(&world, &["config"], Some(&store), false, None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("<secret> (file)"), "{out}");
    assert!(!out.contains(&key) && !err.contains(&key));
}

#[test]
fn canary_a_secret_shaped_model_and_a_bad_accept_remote_are_not_echoed() {
    let world = home("");
    let canary = canaries()[3].clone();
    let (code, out, err) = go(
        &world,
        &[
            "setup",
            "--no-input",
            "--provider",
            "ollama",
            "--model",
            &canary,
        ],
        None,
        false,
        None,
    );
    assert_eq!(code, 2, "{out}");
    assert!(!err.contains(&canary) && !out.contains(&canary), "{err}");
    assert!(!world.config.exists());

    let (code, out, err) = go(
        &world,
        &[
            "setup",
            "--no-input",
            "--provider",
            "ollama",
            "--model",
            "fixture:1",
            "--accept-remote",
            &canary,
        ],
        None,
        false,
        None,
    );
    assert_eq!(code, 2, "{out}");
    assert!(err.contains("expected openrouter"), "{err}");
    assert!(!err.contains(&canary) && !out.contains(&canary));
    assert!(!world.config.exists());

    let (code, _out, err) = go(
        &world,
        &[
            "setup",
            "--no-input",
            "--provider",
            "ollama",
            "--model",
            "latest",
        ],
        None,
        false,
        None,
    );
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("exact model id"), "{err}");
    assert!(!world.config.exists());
}

#[test]
fn a_remote_ollama_host_is_refused_without_echoing_it() {
    let world = home("");
    let mut env = world.env.clone();
    env.insert(
        "OLLAMA_HOST".into(),
        "https://ollama.example.test:11434".into(),
    );
    let io = Io::capture();
    let code = execute_at(
        &[
            "setup",
            "--no-input",
            "--provider",
            "ollama",
            "--model",
            "fixture:1",
        ],
        &RunEnv {
            cwd: world.repo.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
        },
        &io,
    );
    let err = io.stderr_text();
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("loopback"), "{err}");
    assert!(!err.contains("example.test"), "{err}");
    assert!(!io.stdout_text().contains("example.test"));
    assert!(!world.config.exists());
}

#[test]
fn setup_outside_a_repository_exits_before_writing() {
    let bare = TempDir::new("setup-not-a-repo");
    let home = TempDir::new("setup-home-bare");
    let home_path = home.path().canonicalize().unwrap();
    let config = home_path.join("config.toml");
    let env = BTreeMap::from([
        ("HOME".into(), home_path.display().to_string()),
        ("GITCKO_CONFIG".into(), config.display().to_string()),
    ]);
    let io = Io::capture();
    let code = execute_at(
        &[
            "setup",
            "--no-input",
            "--provider",
            "ollama",
            "--model",
            "fixture:1",
        ],
        &RunEnv {
            cwd: bare.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
        },
        &io,
    );
    assert_eq!(code, 2, "{}", io.stdout_text());
    assert!(
        io.stderr_text().contains("not a git repository"),
        "{}",
        io.stderr_text()
    );
    assert!(!config.exists());
}

struct Seen(Mutex<Option<bool>>);

impl ReviewRunner for Seen {
    fn run(&self, job: &ReviewJob<'_>, on_event: &dyn Fn(&RunEvent)) -> Result<Outcome, RunError> {
        *self.0.lock().unwrap() = Some(job.local_only);
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
        review.status = ReviewStatus::Complete;
        review.findings.clear();
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

fn with_history(repo: &Path) {
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.rs"), b"pub fn a() {}\n").unwrap();
    commit(repo, "base");
    assert!(
        git(repo)
            .args(["branch", "base"])
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(repo.join("src/lib.rs"), b"pub fn a() { let _ = 1; }\n").unwrap();
    commit(repo, "head");
}

#[test]
fn a_setup_config_can_run_a_review_and_a_stored_key_is_not_printed() {
    let world = home("");
    with_history(world.repo.path());
    let (code, _out, err) = go(
        &world,
        &[
            "setup",
            "--no-input",
            "--provider",
            "ollama",
            "--model",
            "fixture:1",
        ],
        None,
        false,
        None,
    );
    assert_eq!(code, 0, "{err}");
    let local = Seen(Mutex::new(None));
    let io = Io::capture();
    let code = execute_at(
        &["review", "--base", "base"],
        &RunEnv {
            cwd: world.repo.path(),
            env: &world.env,
            runner: Some(&local),
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
        },
        &io,
    );
    assert_eq!(code, 0, "{}", io.stderr_text());
    assert_eq!(*local.0.lock().unwrap(), Some(true));

    let key = canaries()[1].clone();
    let store = Store::new();
    store.set("openrouter", &key).unwrap();
    let keyed = home("");
    with_history(keyed.repo.path());
    let (code, out, err) = go(
        &keyed,
        &[
            "setup",
            "--no-input",
            "--provider",
            "openrouter",
            "--model",
            "anthropic/claude-haiku-5.5",
            "--accept-remote",
            "openrouter",
        ],
        Some(&store),
        false,
        None,
    );
    assert_eq!(code, 0, "{err}{out}");
    assert!(!err.contains(&key) && !out.contains(&key));
    let remote = Seen(Mutex::new(None));
    let io = Io::capture();
    let code = execute_at(
        &["review", "--base", "base"],
        &RunEnv {
            cwd: keyed.repo.path(),
            env: &keyed.env,
            runner: Some(&remote),
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: Some(&store),
            answers: None,
        },
        &io,
    );
    let err = io.stderr_text();
    let out = io.stdout_text();
    assert_eq!(code, 0, "{err}{out}");
    assert_eq!(*remote.0.lock().unwrap(), Some(false));
    assert!(!err.contains(&key) && !out.contains(&key));
    let remote_at = err.find("selected code is sent to").expect("disclosure");
    let stage_at = err.find("stage:").expect("progress");
    assert!(remote_at < stage_at, "{err}");
}
