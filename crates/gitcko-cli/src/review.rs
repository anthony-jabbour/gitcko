// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko review` for a committed comparison. The command resolves config and refs,
//! then asks a [`ReviewRunner`] to do the work. The real runner calls
//! [`gitcko_review::review_streaming`]; tests pass a runner that returns a stored
//! shape, so exit codes do not need a model.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use gitcko_config::{
    Ceilings, FolderConfig, GitckoPaths, RepoFiles, RunFlags, has_remote_acknowledgement,
    resolve_repo, resolve_user,
};
use gitcko_domain::{RepoPath, ReviewId};
use gitcko_git::Repo;
use gitcko_model::{DestinationClass, ModelProvider as DynProvider, ResolvedModels, resolve};
use gitcko_model_ollama::Ollama;
use gitcko_model_openai::OpenRouter;
use gitcko_protocol::config::{ModelProvider, ModelRole, UserConfig};
use gitcko_protocol::event::{RunEvent, RunEventKind};
use gitcko_protocol::review::{Review, ReviewStatus, Severity};
use gitcko_render::{boxed, status_line, text};
use gitcko_review::{
    LOCAL_DEADLINE, NoFeedback, REMOTE_DEADLINE, ReviewInput, base_run, review_streaming,
};
use gitcko_storage::{ReviewStore, RunningMarker};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::Io;
use crate::cli::{FailOn, LayoutArg, ReviewArgs};
use crate::clock::now_rfc3339;
use crate::exit::review_exit;

/// Why a review did not produce an exit code on its own. The message names a flag or
/// a setup step, never a secret, repository text, or model output.
#[derive(Debug)]
pub struct RunError {
    /// Process exit code.
    pub code: u8,
    /// Safe to print. Names a flag or a setup step, never a secret or model output.
    pub(crate) message: String,
}

impl RunError {
    fn new(code: u8, message: impl Into<String>) -> RunError {
        RunError {
            code,
            message: message.into(),
        }
    }
}

/// Everything the engine needs, already checked. A test runner can ignore the
/// repository and return a canned [`gitcko_review::Outcome`].
pub struct ReviewJob<'a> {
    /// The repository under review.
    pub repo: &'a Repo,
    /// Merge base of the requested base ref and the head.
    pub base: gitcko_git::CommitId,
    /// The head commit.
    pub head: gitcko_git::CommitId,
    /// Id the stored file and the running marker share.
    pub review_id: String,
    /// `--paths`, already checked as repository-relative.
    pub only_paths: Vec<RepoPath>,
    /// `--include`, already checked the same way.
    pub include_paths: Vec<RepoPath>,
    /// Bindings from the user config only. Repository `[models]` never arrives here.
    pub models: ResolvedModels,
    /// True when every binding stays on this machine, so the router must not fall
    /// back to a remote one.
    pub local_only: bool,
    /// Tripped by Ctrl-C. The engine stores a canceled review when this fires.
    pub cancel: CancellationToken,
}

/// Runs one review. The default is the real engine; tests supply a fake so a
/// partial result can be forced without a model.
pub trait ReviewRunner: Send + Sync {
    /// Produce the review. `on_event` is called as steps start, including by a fake
    /// that wants the progress line covered.
    fn run(
        &self,
        job: &ReviewJob<'_>,
        on_event: &dyn Fn(&RunEvent),
    ) -> Result<gitcko_review::Outcome, RunError>;
}

/// The engine behind `gitcko review`. Builds one provider per binding and streams
/// step events to the terminal.
pub struct EngineRunner;

impl ReviewRunner for EngineRunner {
    fn run(
        &self,
        job: &ReviewJob<'_>,
        on_event: &dyn Fn(&RunEvent),
    ) -> Result<gitcko_review::Outcome, RunError> {
        let mut providers = BTreeMap::new();
        for (name, binding) in &job.models.bindings {
            let provider: Arc<dyn DynProvider> = match binding.provider {
                ModelProvider::Ollama => Arc::new(
                    Ollama::new(binding.clone())
                        .map_err(|_| RunError::new(1, "could not use the ollama binding"))?,
                ),
                ModelProvider::Openrouter => Arc::new(
                    OpenRouter::new(binding.clone())
                        .map_err(|_| RunError::new(1, "could not use the openrouter binding"))?,
                ),
            };
            providers.insert(name.clone(), provider);
        }
        let router = gitcko_model::Router::new(job.models.clone(), providers);
        let deadline = if job.local_only {
            LOCAL_DEADLINE
        } else {
            REMOTE_DEADLINE
        };
        let mut run = base_run(job.review_id.clone(), Some(deadline));
        run.local_only = job.local_only;
        run.cancel = job.cancel.clone();
        let now = now_rfc3339;
        let input = ReviewInput {
            repo: job.repo,
            base: job.base,
            head: job.head,
            now: &now,
            only_paths: &job.only_paths,
            include_paths: &job.include_paths,
        };
        let cancel = job.cancel.clone();
        // `block_on` polls the review on this thread. The signal task is the one that
        // moves to the pool. https://docs.rs/tokio/1.54.1/tokio/signal/fn.ctrl_c.html
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| RunError::new(1, "could not start the runtime"))?;
        let outcome = runtime.block_on(async move {
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    cancel.cancel();
                }
            });
            review_streaming(input, &router, run, &NoFeedback, on_event).await
        });
        Ok(outcome)
    }
}

/// One stage line for stderr. Only `step.started` is printed: the completed event
/// repeats the same stage, and the status line carries the outcome.
pub fn progress_line(event: &RunEvent) -> Option<String> {
    match &event.kind {
        RunEventKind::StepStarted { step } => Some(format!("stage: {step}")),
        _ => None,
    }
}

/// Run `gitcko review` (not `show` or `status`).
pub fn run<O, E>(
    args: &ReviewArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    runner: Option<&dyn ReviewRunner>,
    tty: bool,
    io: &Io<O, E>,
) -> u8
where
    O: Write + Send,
    E: Write + Send,
{
    if let Some(code) = debug_seam(args, env, tty, io) {
        return code;
    }
    match run_inner(args, cwd, env, runner, tty, io) {
        Ok(code) => code,
        Err(err) => {
            let _ = writeln!(lock(&io.stderr), "{}", err.message);
            err.code
        }
    }
}

fn run_inner<O, E>(
    args: &ReviewArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    runner: Option<&dyn ReviewRunner>,
    tty: bool,
    io: &Io<O, E>,
) -> Result<u8, RunError>
where
    O: Write + Send,
    E: Write + Send,
{
    // `--no-input` does not change this build: an unacknowledged remote is exit 2
    // either way until setup can prompt. `--context` and the color flags parse, and
    // this build prints neither source lines nor ANSI.
    let _ = (args.no_input, args.context, args.color, args.no_color);
    let repo = Repo::open(cwd).map_err(|_| RunError::new(2, "not a git repository"))?;
    let head_ref = args.head.as_deref().unwrap_or("HEAD");
    let base_ref = args.base.as_deref().unwrap_or("origin/HEAD");
    let head = repo
        .resolve(head_ref)
        .map_err(|_| RunError::new(2, "could not resolve the head ref; pass --head"))?;
    let base_tip = repo.resolve(base_ref).map_err(|_| {
        RunError::new(
            2,
            "could not resolve the base ref; pass --base with a commit in this repository",
        )
    })?;
    let base = repo
        .merge_base(base_tip, head)
        .map_err(|_| RunError::new(2, "could not find a merge base; pass --base"))?
        .ok_or_else(|| RunError::new(2, "those commits have no merge base; pass --base"))?;
    let only_paths = parse_paths(&args.paths)?;
    let include_paths = parse_paths(&args.include)?;
    reject_repo_models(cwd)?;
    let loaded = load_models(env)?;
    if let Some(provider) = loaded.unacked {
        return Err(RunError::new(
            2,
            format!("run `gitcko setup --accept-remote {provider}`"),
        ));
    }
    if let Some(flag) = unavailable(args) {
        return Err(RunError::new(
            6,
            format!("notice: {flag} is not available in this build"),
        ));
    }
    for path in &include_paths {
        let _ = writeln!(lock(&io.stderr), "include: {}", path.as_str());
    }
    disclose(&loaded.models, io);
    let paths = GitckoPaths::from_env(env).map_err(|err| RunError::new(2, err.to_string()))?;
    let repo_id = repo_cache_id(cwd)?;
    let review_id = ReviewId::new().to_string();
    let store = ReviewStore::new(&paths.cache_dir);
    store
        .mark_running(
            &repo_id,
            &review_id,
            &RunningMarker {
                pid: std::process::id(),
                started: now_rfc3339(),
                head_sha: Some(head.to_string()),
                base_sha: Some(base.to_string()),
            },
        )
        .map_err(|_| RunError::new(1, "could not record that the review is running"))?;
    let local_only = loaded
        .models
        .bindings
        .values()
        .all(|binding| binding.destination.class == DestinationClass::OnDevice);
    let cancel = CancellationToken::new();
    let job = ReviewJob {
        repo: &repo,
        base,
        head,
        review_id: review_id.clone(),
        only_paths,
        include_paths,
        models: loaded.models,
        local_only,
        cancel,
    };
    let stderr = Arc::clone(&io.stderr);
    let on_event = move |event: &RunEvent| {
        if let Some(line) = progress_line(event) {
            let _ = writeln!(lock(&stderr), "{line}");
        }
    };
    let engine = EngineRunner;
    let runner: &dyn ReviewRunner = runner.unwrap_or(&engine);
    let outcome = match runner.run(&job, &on_event) {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = store.clear_running(&repo_id, &review_id);
            return Err(err);
        }
    };
    if store.put(&repo_id, &outcome.review).is_err() {
        let _ = writeln!(lock(&io.stderr), "could not store the review");
    }
    let _ = store.clear_running(&repo_id, &review_id);
    write_events(&store, &repo_id, &outcome.review.review_id, &outcome.events);
    present(&outcome.review, args, tty, terminal_width(args, env), io)?;
    Ok(review_exit(
        outcome.review.status,
        &outcome.review.findings,
        fail_on(args),
    ))
}

struct Loaded {
    models: ResolvedModels,
    unacked: Option<&'static str>,
}

fn load_models(env: &BTreeMap<String, String>) -> Result<Loaded, RunError> {
    let paths = GitckoPaths::from_env(env).map_err(|err| RunError::new(2, err.to_string()))?;
    let text = match fs::read_to_string(&paths.config_file) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(RunError::new(2, "could not read the user config")),
    };
    let resolved = resolve_user(text.as_deref(), env, &RunFlags::default())
        .map_err(|err| RunError::new(2, err.to_string()))?;
    let user = resolved.user_config();
    let models = user.models.clone().unwrap_or_default();
    let models = resolve(&models, env).map_err(|_| RunError::new(2, "run `gitcko setup`"))?;
    if models
        .require(&[ModelRole::Review, ModelRole::Judge])
        .is_err()
    {
        return Err(RunError::new(2, "run `gitcko setup`"));
    }
    let unacked = unacknowledged(&user, &models);
    Ok(Loaded { models, unacked })
}

fn unacknowledged(user: &UserConfig, models: &ResolvedModels) -> Option<&'static str> {
    models.bindings.values().find_map(|binding| {
        if binding.destination.class != DestinationClass::CustomerRemote {
            return None;
        }
        let name = match binding.provider {
            ModelProvider::Openrouter => "openrouter",
            ModelProvider::Ollama => "ollama",
        };
        if binding.provider == ModelProvider::Openrouter
            && has_remote_acknowledgement(user, binding.provider)
        {
            None
        } else {
            Some(name)
        }
    })
}

fn reject_repo_models(cwd: &Path) -> Result<(), RunError> {
    let path = cwd.join(".gitcko").join("config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(RunError::new(2, "could not read repository config")),
    };
    let files = RepoFiles {
        folders: vec![FolderConfig {
            folder: None,
            config_toml: Some(text),
            rules_md: None,
        }],
        legacy_root_file: false,
    };
    let reviewed = RepoPath::parse("README").map_err(|_| RunError::new(2, "invalid path"))?;
    resolve_repo(&files, &reviewed, &RunFlags::default(), &Ceilings::local())
        .map(|_| ())
        .map_err(|err| RunError::new(2, err.to_string()))
}

fn parse_paths(raw: &[String]) -> Result<Vec<RepoPath>, RunError> {
    raw.iter()
        .map(|path| {
            RepoPath::parse(path)
                .map_err(|_| RunError::new(2, "a path must be repository-relative"))
        })
        .collect()
}

fn unavailable(args: &ReviewArgs) -> Option<&'static str> {
    if args.plus {
        Some("--plus")
    } else if args.max {
        Some("--max")
    } else if args.auto {
        Some("--auto")
    } else if args.cloud {
        Some("--cloud")
    } else if args.verify {
        Some("--verify")
    } else if args.instructions.is_some() {
        Some("--instructions")
    } else if args.resume {
        Some("--resume")
    } else if args.layout == Some(LayoutArg::Diff) {
        Some("--layout diff")
    } else {
        None
    }
}

fn fail_on(args: &ReviewArgs) -> Option<Severity> {
    args.fail_on.map(|level| match level {
        FailOn::P0 => Severity::P0,
        FailOn::P1 => Severity::P1,
        FailOn::P2 => Severity::P2,
    })
}

fn disclose<O, E>(models: &ResolvedModels, io: &Io<O, E>)
where
    O: Write,
    E: Write,
{
    let remote = models
        .bindings
        .values()
        .any(|binding| binding.destination.class == DestinationClass::CustomerRemote);
    if !remote {
        let _ = writeln!(lock(&io.stderr), "local: code stays on this machine");
        return;
    }
    if let Some(binding) = models.binding_for(ModelRole::Review) {
        let provider = match binding.provider {
            ModelProvider::Openrouter => "openrouter",
            ModelProvider::Ollama => "ollama",
        };
        let _ = writeln!(
            lock(&io.stderr),
            "remote: selected code is sent to {provider} ({})",
            binding.model
        );
    }
}

fn present<O, E>(
    review: &Review,
    args: &ReviewArgs,
    tty: bool,
    width: u16,
    io: &Io<O, E>,
) -> Result<(), RunError>
where
    O: Write,
    E: Write,
{
    if review.status != ReviewStatus::Complete {
        let _ = writeln!(lock(&io.stderr), "{}", status_line(review));
    }
    let mut stdout = lock(&io.stdout);
    if args.json {
        let json = serde_json::to_string(review)
            .map_err(|_| RunError::new(1, "could not encode the review"))?;
        let _ = writeln!(stdout, "{json}");
    } else if args.text || args.agent || !tty {
        let _ = write!(stdout, "{}", text(review));
    } else {
        let _ = write!(stdout, "{}", boxed(review, width));
    }
    Ok(())
}

fn terminal_width(args: &ReviewArgs, env: &BTreeMap<String, String>) -> u16 {
    if let Some(width) = args.width {
        return width;
    }
    env.get("COLUMNS")
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(80)
        .clamp(40, 240)
}

/// Cache key for this checkout: SHA-256 of the canonical working directory.
/// `review`, `review show` and `review status` must agree, and the key is the path
/// rather than a remote URL because the opened repo does not expose one.
pub fn repo_cache_id(cwd: &Path) -> Result<String, RunError> {
    let canonical = cwd
        .canonicalize()
        .map_err(|_| RunError::new(2, "could not resolve the repository path"))?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    Ok(hex_encode(&hasher.finalize()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn write_events(store: &ReviewStore, repo_id: &str, review_id: &str, events: &[RunEvent]) {
    if events.is_empty() {
        return;
    }
    let path = store
        .root()
        .join("repos")
        .join(repo_id)
        .join("reviews")
        .join(format!("{review_id}.events"));
    if let Ok(bytes) = serde_json::to_vec(events) {
        let _ = fs::write(path, bytes);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

/// Debug builds only. `GITCKO_TEST_SEAM=1` plus a review file or a cancel wait lets
/// the binary's process exit be tested without a model. Release builds compile this
/// to nothing, so a production binary never reads those variables.
fn debug_seam<O, E>(
    args: &ReviewArgs,
    env: &BTreeMap<String, String>,
    tty: bool,
    io: &Io<O, E>,
) -> Option<u8>
where
    O: Write + Send,
    E: Write + Send,
{
    #[cfg(not(debug_assertions))]
    {
        let _ = (args, env, tty, io);
        None
    }
    #[cfg(debug_assertions)]
    {
        if env.get("GITCKO_TEST_SEAM").map(String::as_str) != Some("1") {
            return None;
        }
        if env.get("GITCKO_TEST_WAIT_CANCEL").map(String::as_str) == Some("1") {
            return Some(wait_for_cancel(io));
        }
        let Some(path) = env.get("GITCKO_TEST_REVIEW_JSON") else {
            return Some(2);
        };
        let Ok(text) = fs::read_to_string(path) else {
            return Some(2);
        };
        let Ok(review) = serde_json::from_str::<Review>(&text) else {
            return Some(2);
        };
        if present(&review, args, tty, args.width.unwrap_or(80), io).is_err() {
            return Some(1);
        }
        Some(review_exit(review.status, &review.findings, fail_on(args)))
    }
}

#[cfg(debug_assertions)]
fn wait_for_cancel<O, E>(io: &Io<O, E>) -> u8
where
    O: Write + Send,
    E: Write + Send,
{
    let _ = writeln!(lock(&io.stderr), "waiting for cancel");
    let _ = lock(&io.stderr).flush();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return 1,
    };
    runtime.block_on(async {
        let cancel = CancellationToken::new();
        let flag = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                flag.cancel();
            }
        });
        cancel.cancelled().await;
    });
    let _ = writeln!(lock(&io.stderr), "status: canceled");
    130
}
