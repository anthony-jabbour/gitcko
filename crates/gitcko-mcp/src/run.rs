// SPDX-License-Identifier: AGPL-3.0-or-later

//! One tool call: resolve the checkout, read or run a review, and shape the result.
//! Model routing uses the user config only. Repository `[models]` never starts a run.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gitcko_config::{
    Ceilings, FolderConfig, GitckoPaths, RepoFiles, RunFlags, has_remote_acknowledgement,
    resolve_repo, resolve_user,
};
use gitcko_domain::{RepoPath, ReviewId};
use gitcko_git::Repo;
use gitcko_model::ModelProvider as Provider;
use gitcko_model::{DestinationClass, ResolvedModels, resolve};
use gitcko_model_ollama::Ollama;
use gitcko_model_openai::OpenRouter;
use gitcko_protocol::config::{ModelProvider as ProviderKind, ModelRole, UserConfig};
use gitcko_protocol::event::RunEvent;
use gitcko_protocol::review::{Review, ReviewStatus, Tier};
use gitcko_review::{
    LOCAL_DEADLINE, NoFeedback, REMOTE_DEADLINE, ReviewInput, base_run, review_streaming,
};
use gitcko_storage::{ReviewStore, RunState, RunningMarker, StoreError};
use tokio_util::sync::CancellationToken;

use crate::args::ToolArgs;
use crate::clock::now_rfc3339;
use crate::paths::root_id;
use crate::present::{
    self, PAGE, cursor, filter_key, finding_value, is_addressed, lifecycle_of, list_history,
    page_text, parse_cursor, review_text, tool_error, tool_ok,
};
use crate::scope::write_scope;

pub type ProviderMap = BTreeMap<String, Arc<dyn Provider>>;
pub type Script = Arc<dyn Fn(&ReviewJob) -> Result<gitcko_review::Outcome, String> + Send + Sync>;

/// Forwards a real engine event. `Send` so a review running off the handler task can report it.
pub type Report = Arc<dyn Fn(&RunEvent) + Send + Sync>;

#[derive(Clone)]
pub enum Engine {
    /// Real router. `Some` replaces the adapters (tests); `None` builds them.
    Live(Option<ProviderMap>),
    Script(Script),
}

/// What a review is bound to, after refs have been resolved once.
#[derive(Clone)]
pub struct ReviewJob {
    pub cwd: PathBuf,
    pub base_sha: String,
    pub head_sha: String,
    pub review_id: String,
    pub only_paths: Vec<RepoPath>,
    pub local_only: bool,
    pub(crate) models: ResolvedModels,
}

pub enum Fail {
    Tool(rmcp::model::CallToolResult),
}

pub async fn dispatch(
    root: &Path,
    env: &BTreeMap<String, String>,
    engine: &Engine,
    args: ToolArgs,
    cancel: &CancellationToken,
    on_event: &Report,
    log: &mut dyn FnMut(&str),
) -> Result<rmcp::model::CallToolResult, Fail> {
    let repo_key = root_id(root).map_err(|_| internal("could not resolve the repository path"))?;
    match args {
        ToolArgs::Review { base, head, paths } => {
            review(
                root, env, engine, &repo_key, base, head, paths, cancel, on_event, log,
            )
            .await
        }
        ToolArgs::Status { review_id, head } => status(root, env, &repo_key, review_id, head),
        ToolArgs::Get { review_id } => get(env, &repo_key, &review_id),
        ToolArgs::List {
            review_id,
            addressed,
            has_suggestion,
            severity,
            path,
            limit,
            cursor,
        } => list(
            root,
            env,
            &repo_key,
            review_id,
            addressed,
            has_suggestion,
            severity,
            path,
            limit,
            cursor,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
async fn review(
    root: &Path,
    env: &BTreeMap<String, String>,
    engine: &Engine,
    repo_key: &str,
    base: Option<String>,
    head: Option<String>,
    paths: Vec<RepoPath>,
    cancel: &CancellationToken,
    on_event: &Report,
    log: &mut dyn FnMut(&str),
) -> Result<rmcp::model::CallToolResult, Fail> {
    let head_ref = head.as_deref().unwrap_or("HEAD");
    let base_ref = base.as_deref().unwrap_or("origin/HEAD");
    // `Repo` is not `Send`. Resolve the range before the first await so the
    // handler future the SDK spawns stays sendable.
    let (head_id, base_id) = {
        let repo = Repo::open(root).map_err(|_| snapshot())?;
        let head_id = repo.resolve(head_ref).map_err(|_| snapshot())?;
        let base_tip = repo.resolve(base_ref).map_err(|_| snapshot())?;
        let base_id = repo
            .merge_base(base_tip, head_id)
            .map_err(|_| snapshot())?
            .ok_or_else(snapshot)?;
        (head_id, base_id)
    };
    reject_repo_models(root)?;
    let loaded = load_models(env)?;
    if let Some(provider) = loaded.unacked {
        return Err(capability(ack_message(provider)));
    }
    disclose(&loaded.models, log);
    let paths_cfg =
        GitckoPaths::from_env(env).map_err(|_| internal("could not read the user config"))?;
    let store = ReviewStore::new(&paths_cfg.cache_dir);
    let review_id = ReviewId::new().to_string();
    let local_only = loaded
        .models
        .bindings
        .values()
        .all(|binding| binding.destination.class == DestinationClass::OnDevice);
    store
        .mark_running(
            repo_key,
            &review_id,
            &RunningMarker {
                pid: std::process::id(),
                started: now_rfc3339(),
                head_sha: Some(head_id.to_string()),
                base_sha: Some(base_id.to_string()),
            },
        )
        .map_err(|_| internal("could not record that the review is running"))?;
    let job = ReviewJob {
        cwd: root.to_path_buf(),
        base_sha: base_id.to_string(),
        head_sha: head_id.to_string(),
        review_id: review_id.clone(),
        only_paths: paths.clone(),
        local_only,
        models: loaded.models,
    };
    let outcome = match execute(engine, &job, cancel, on_event).await {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = store.clear_running(repo_key, &review_id);
            return Err(err);
        }
    };
    let mut review = outcome.review;
    review.review_id = review_id.clone();
    review.target.base_sha = job.base_sha;
    review.target.head_sha = job.head_sha;
    review.target.local_changes = false;
    review.tier = Tier::Base;
    if review.status != ReviewStatus::Complete {
        review.score = None;
    }
    // The router may surface cancellation as the provider's own `Canceled`
    // error. The client already dropped the request, so the stored review is
    // canceled either way and no later role is authoritative.
    if cancel.is_cancelled() {
        review.status = ReviewStatus::Canceled;
        if !review.reason_codes.iter().any(|code| code == "canceled") {
            review.reason_codes.insert(0, "canceled".into());
        }
        review.score = None;
    }
    if store.put(repo_key, &review).is_err() {
        let _ = store.clear_running(repo_key, &review_id);
        return Err(internal("could not store the review"));
    }
    let _ = store.clear_running(repo_key, &review_id);
    write_events(&store, repo_key, &review.review_id, &outcome.events);
    write_scope(&store, repo_key, &review.review_id, &paths);
    let echoed: Vec<String> = paths.iter().map(|path| path.as_str().to_string()).collect();
    let page = page_slice(&store, repo_key, &review, 0, PAGE);
    Ok(tool_ok(
        page_text(&review, page.slice),
        present::review_body(
            &review,
            repo_key,
            &echoed,
            page.findings,
            page.truncated,
            page.next,
        ),
    ))
}

async fn execute(
    engine: &Engine,
    job: &ReviewJob,
    cancel: &CancellationToken,
    on_event: &Report,
) -> Result<gitcko_review::Outcome, Fail> {
    match engine {
        Engine::Script(script) => {
            script(job).map_err(|_| internal("the review could not be completed"))
        }
        Engine::Live(providers) => {
            live_blocking(
                job.clone(),
                providers.clone(),
                cancel.clone(),
                Arc::clone(on_event),
            )
            .await
        }
    }
}

async fn live_blocking(
    job: ReviewJob,
    providers: Option<ProviderMap>,
    cancel: CancellationToken,
    on_event: Report,
) -> Result<gitcko_review::Outcome, Fail> {
    let providers = match providers {
        Some(map) => map,
        None => build_providers(&job.models)?,
    };
    tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| internal("the review could not be completed"))?;
        runtime.block_on(async move {
            let repo =
                Repo::open(&job.cwd).map_err(|_| internal("the review could not be completed"))?;
            let base = repo
                .resolve(&job.base_sha)
                .map_err(|_| internal("the review could not be completed"))?;
            let head = repo
                .resolve(&job.head_sha)
                .map_err(|_| internal("the review could not be completed"))?;
            let router = gitcko_model::Router::new(job.models, providers);
            let deadline = if job.local_only {
                LOCAL_DEADLINE
            } else {
                REMOTE_DEADLINE
            };
            let mut run = base_run(job.review_id, Some(deadline));
            run.local_only = job.local_only;
            run.cancel = cancel;
            let now = now_rfc3339;
            let input = ReviewInput {
                repo: &repo,
                base,
                head,
                now: &now,
                only_paths: &job.only_paths,
                include_paths: &[],
            };
            Ok(review_streaming(input, &router, run, &NoFeedback, &|event| on_event(event)).await)
        })
    })
    .await
    .map_err(|_| internal("the review could not be completed"))?
}

fn build_providers(models: &ResolvedModels) -> Result<ProviderMap, Fail> {
    let mut providers = BTreeMap::new();
    for (name, binding) in &models.bindings {
        let provider: Arc<dyn Provider> = match binding.provider {
            ProviderKind::Ollama => {
                Arc::new(Ollama::new(binding.clone()).map_err(|_| model_unavailable())?)
            }
            ProviderKind::Openrouter => {
                Arc::new(OpenRouter::new(binding.clone()).map_err(|_| model_unavailable())?)
            }
        };
        providers.insert(name.clone(), provider);
    }
    Ok(providers)
}

fn status(
    root: &Path,
    env: &BTreeMap<String, String>,
    repo_key: &str,
    review_id: Option<String>,
    head: Option<String>,
) -> Result<rmcp::model::CallToolResult, Fail> {
    let store = open_store(env)?;
    let repo = Repo::open(root).map_err(|_| snapshot())?;
    let selected = select(&store, repo_key, &repo, review_id, head)?;
    match selected {
        Selected::Review(review) => {
            let moved = head_moved(&repo, &review.target.head_sha);
            let addressed = review
                .findings
                .iter()
                .filter(|finding| is_addressed(&store, repo_key, &review, finding))
                .count();
            Ok(tool_ok(
                review_text(&review),
                present::status_body(
                    &review,
                    repo_key,
                    lifecycle_of(review.status),
                    review.findings.len(),
                    addressed,
                    moved,
                ),
            ))
        }
        Selected::Running {
            review_id,
            base_sha,
            head_sha,
        } => Ok(tool_ok(
            "INCOMPLETE review (in_progress)\n".to_string(),
            present::marker_body(
                &review_id,
                repo_key,
                &base_sha,
                &head_sha,
                "partial",
                &["in_progress".to_string()],
                "investigating",
                head_moved(&repo, &head_sha),
            ),
        )),
        Selected::Stale {
            review_id,
            base_sha,
            head_sha,
        } => Ok(tool_ok(
            "status: failed\n".to_string(),
            present::marker_body(
                &review_id,
                repo_key,
                &base_sha,
                &head_sha,
                "failed",
                &[],
                "failed",
                head_moved(&repo, &head_sha),
            ),
        )),
    }
}

fn get(
    env: &BTreeMap<String, String>,
    repo_key: &str,
    review_id: &str,
) -> Result<rmcp::model::CallToolResult, Fail> {
    let store = open_store(env)?;
    let review = load_review(&store, repo_key, review_id)?;
    let page = page_slice(&store, repo_key, &review, 0, PAGE);
    Ok(tool_ok(
        page_text(&review, page.slice),
        present::get_body(&review, repo_key, page.findings, page.truncated, page.next),
    ))
}

#[allow(clippy::too_many_arguments)]
fn list(
    root: &Path,
    env: &BTreeMap<String, String>,
    repo_key: &str,
    review_id: Option<String>,
    addressed: Option<bool>,
    has_suggestion: Option<bool>,
    severity: Option<Vec<String>>,
    path: Option<RepoPath>,
    limit: usize,
    cursor_text: Option<String>,
) -> Result<rmcp::model::CallToolResult, Fail> {
    let store = open_store(env)?;
    let review = match review_id {
        Some(id) => load_review(&store, repo_key, &id)?,
        None => {
            let repo = Repo::open(root).map_err(|_| snapshot())?;
            match select(&store, repo_key, &repo, None, None)? {
                Selected::Review(review) => *review,
                Selected::Running { .. } | Selected::Stale { .. } => {
                    return Err(not_found());
                }
            }
        }
    };
    let history = list_history(&store, repo_key);
    let key = filter_key(
        addressed,
        has_suggestion,
        severity.as_deref(),
        path.as_ref().map(RepoPath::as_str),
        limit,
        &history,
    );
    let offset = match cursor_text {
        None => 0,
        Some(text) => {
            let (id, cursor_key, offset) = parse_cursor(&text).ok_or_else(invalid)?;
            if id != review.review_id || cursor_key != key {
                return Err(invalid());
            }
            offset
        }
    };
    let path_text = path.as_ref().map(RepoPath::as_str);
    let filtered: Vec<&gitcko_protocol::review::Finding> = review
        .findings
        .iter()
        .filter(|finding| {
            severity.as_ref().is_none_or(|levels| {
                levels
                    .iter()
                    .any(|level| level == &severity_name(finding.severity))
            }) && path_text.is_none_or(|wanted| finding.path == wanted)
                && addressed
                    .is_none_or(|wanted| is_addressed(&store, repo_key, &review, finding) == wanted)
                && has_suggestion.is_none_or(|wanted| finding.suggestion.is_some() == wanted)
        })
        .collect();
    let start = offset.min(filtered.len());
    let end = start.saturating_add(limit).min(filtered.len());
    let truncated = end < filtered.len();
    let next = truncated.then(|| cursor(&review.review_id, &key, end));
    let findings = filtered[start..end]
        .iter()
        .map(|finding| {
            finding_value(
                &review,
                finding,
                is_addressed(&store, repo_key, &review, finding),
            )
        })
        .collect();
    let slice: Vec<_> = filtered[start..end]
        .iter()
        .map(|finding| (*finding).clone())
        .collect();
    Ok(tool_ok(
        page_text(&review, &slice),
        present::list_body(&review, repo_key, findings, limit, truncated, next),
    ))
}

enum Selected {
    Review(Box<Review>),
    Running {
        review_id: String,
        base_sha: String,
        head_sha: String,
    },
    Stale {
        review_id: String,
        base_sha: String,
        head_sha: String,
    },
}

fn select(
    store: &ReviewStore,
    repo_key: &str,
    repo: &Repo,
    review_id: Option<String>,
    head: Option<String>,
) -> Result<Selected, Fail> {
    if let Some(id) = review_id {
        let review = match store.get(repo_key, &id) {
            Ok(review) => Some(review),
            Err(StoreError::NotFound) => None,
            Err(_) => return Err(internal("could not read the stored review")),
        };
        if let Some(head_ref) = head.as_deref() {
            let head_sha = repo.resolve(head_ref).map_err(|_| snapshot())?.to_string();
            if let Some(review) = review.as_ref()
                && review.target.head_sha != head_sha
            {
                return Err(invalid_message(
                    "review_id and head do not name the same snapshot",
                ));
            }
            if review.is_none() {
                return Err(not_found());
            }
        }
        if let Some(review) = review {
            return Ok(Selected::Review(Box::new(review)));
        }
        return marker_or_missing(store, repo_key, &id);
    }
    let head_sha = match head {
        Some(head_ref) => repo.resolve(&head_ref).map_err(|_| snapshot())?.to_string(),
        None => repo.resolve("HEAD").map_err(|_| snapshot())?.to_string(),
    };
    let reviews = store
        .list(repo_key)
        .map_err(|_| internal("could not read the stored review"))?;
    if let Some(summary) = reviews.into_iter().find(|item| item.head_sha == head_sha) {
        return Ok(Selected::Review(Box::new(load_review(
            store,
            repo_key,
            &summary.review_id,
        )?)));
    }
    let markers = store
        .list_markers(repo_key)
        .map_err(|_| internal("could not read the stored review"))?;
    for marker in markers {
        let sha = match &marker.state {
            RunState::Running(body) | RunState::Stale(body) => body.head_sha.clone(),
            RunState::Idle => None,
        };
        if sha.as_deref() == Some(head_sha.as_str()) {
            return marker_or_missing(store, repo_key, &marker.review_id);
        }
    }
    Err(not_found())
}

fn marker_or_missing(store: &ReviewStore, repo_key: &str, id: &str) -> Result<Selected, Fail> {
    match store.run_state(repo_key, id) {
        Ok(RunState::Running(body)) => Ok(Selected::Running {
            review_id: id.to_string(),
            base_sha: body.base_sha.unwrap_or_default(),
            head_sha: body.head_sha.unwrap_or_default(),
        }),
        Ok(RunState::Stale(body)) => Ok(Selected::Stale {
            review_id: id.to_string(),
            base_sha: body.base_sha.unwrap_or_default(),
            head_sha: body.head_sha.unwrap_or_default(),
        }),
        Ok(RunState::Idle) => Err(not_found()),
        Err(StoreError::NotFound) => Err(not_found()),
        Err(_) => Err(internal("could not read the stored review")),
    }
}

struct PagedFindings<'a> {
    slice: &'a [gitcko_protocol::review::Finding],
    findings: Vec<serde_json::Value>,
    truncated: bool,
    next: Option<String>,
}

fn page_slice<'a>(
    store: &ReviewStore,
    repo_key: &str,
    review: &'a Review,
    offset: usize,
    limit: usize,
) -> PagedFindings<'a> {
    let history = list_history(store, repo_key);
    let key = filter_key(None, None, None, None, limit, &history);
    let end = offset.saturating_add(limit).min(review.findings.len());
    let start = offset.min(review.findings.len());
    let slice = &review.findings[start..end];
    let findings = slice
        .iter()
        .map(|finding| {
            finding_value(
                review,
                finding,
                is_addressed(store, repo_key, review, finding),
            )
        })
        .collect();
    let truncated = end < review.findings.len();
    let next = truncated.then(|| cursor(&review.review_id, &key, end));
    PagedFindings {
        slice,
        findings,
        truncated,
        next,
    }
}

fn load_review(store: &ReviewStore, repo_key: &str, review_id: &str) -> Result<Review, Fail> {
    match store.get(repo_key, review_id) {
        Ok(review) => Ok(review),
        Err(StoreError::NotFound) => Err(not_found()),
        Err(_) => Err(internal("could not read the stored review")),
    }
}

fn open_store(env: &BTreeMap<String, String>) -> Result<ReviewStore, Fail> {
    let paths =
        GitckoPaths::from_env(env).map_err(|_| internal("could not read the user config"))?;
    Ok(ReviewStore::new(&paths.cache_dir))
}

fn head_moved(repo: &Repo, review_head: &str) -> bool {
    repo.resolve("HEAD")
        .map(|head| head.to_string() != review_head)
        .unwrap_or(false)
}

fn severity_name(severity: gitcko_protocol::review::Severity) -> String {
    serde_json::to_value(severity)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

struct Loaded {
    models: ResolvedModels,
    unacked: Option<&'static str>,
}

fn load_models(env: &BTreeMap<String, String>) -> Result<Loaded, Fail> {
    let paths =
        GitckoPaths::from_env(env).map_err(|_| internal("could not read the user config"))?;
    let text = match fs::read_to_string(&paths.config_file) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(internal("could not read the user config")),
    };
    let resolved = resolve_user(text.as_deref(), env, &RunFlags::default())
        .map_err(|_| internal("could not read the user config"))?;
    let user = resolved.user_config();
    if let Some(provider) = unacked_from_config(&user) {
        return Ok(Loaded {
            models: ResolvedModels {
                bindings: BTreeMap::new(),
                roles: BTreeMap::new(),
                fallbacks: BTreeMap::new(),
                inversion: true,
            },
            unacked: Some(provider),
        });
    }
    let models = resolve(user.models.as_ref().unwrap_or(&Default::default()), env)
        .map_err(|_| model_unavailable())?;
    let unacked = unacknowledged(&user, &models);
    if unacked.is_some() {
        return Ok(Loaded { models, unacked });
    }
    if models
        .require(&[ModelRole::Review, ModelRole::Judge])
        .is_err()
    {
        return Err(model_unavailable());
    }
    Ok(Loaded {
        models,
        unacked: None,
    })
}

fn unacked_from_config(user: &UserConfig) -> Option<&'static str> {
    let models = user.models.as_ref()?.bindings.as_ref()?;
    for binding in models.values() {
        if binding.provider == ProviderKind::Openrouter
            && !has_remote_acknowledgement(user, binding.provider)
        {
            return Some("openrouter");
        }
    }
    None
}

fn unacknowledged(user: &UserConfig, models: &ResolvedModels) -> Option<&'static str> {
    models.bindings.values().find_map(|binding| {
        if binding.destination.class != DestinationClass::CustomerRemote {
            return None;
        }
        let name = match binding.provider {
            ProviderKind::Openrouter => "openrouter",
            ProviderKind::Ollama => "ollama",
        };
        if binding.provider == ProviderKind::Openrouter
            && has_remote_acknowledgement(user, binding.provider)
        {
            None
        } else {
            Some(name)
        }
    })
}

fn ack_message(provider: &str) -> &'static str {
    match provider {
        "openrouter" => "run gitcko setup --accept-remote openrouter",
        _ => "run gitcko setup --accept-remote ollama",
    }
}

fn reject_repo_models(cwd: &Path) -> Result<(), Fail> {
    let path = cwd.join(".gitcko").join("config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(internal("could not read repository config")),
    };
    let files = RepoFiles {
        folders: vec![FolderConfig {
            folder: None,
            config_toml: Some(text),
            rules_md: None,
        }],
        legacy_root_file: false,
    };
    let reviewed =
        RepoPath::parse("README").map_err(|_| internal("could not read repository config"))?;
    match resolve_repo(&files, &reviewed, &RunFlags::default(), &Ceilings::local()) {
        Ok(_) => Ok(()),
        Err(err) if err.to_string().contains("models") => {
            Err(invalid_message("models can't be set in repository config"))
        }
        Err(_) => Err(internal("could not read repository config")),
    }
}

fn disclose(models: &ResolvedModels, log: &mut dyn FnMut(&str)) {
    let remote = models
        .bindings
        .values()
        .any(|binding| binding.destination.class == DestinationClass::CustomerRemote);
    if !remote {
        log("local: code stays on this machine");
        return;
    }
    if let Some(binding) = models.binding_for(ModelRole::Review) {
        let provider = match binding.provider {
            ProviderKind::Openrouter => "openrouter",
            ProviderKind::Ollama => "ollama",
        };
        log(&format!(
            "remote: selected code is sent to {provider} ({})",
            binding.model
        ));
    }
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

fn snapshot() -> Fail {
    Fail::Tool(tool_error(
        "SNAPSHOT_NOT_FOUND",
        "could not resolve the review range",
        false,
    ))
}

fn not_found() -> Fail {
    Fail::Tool(tool_error(
        "REVIEW_NOT_FOUND",
        "no stored review matches that request",
        false,
    ))
}

fn invalid() -> Fail {
    Fail::Tool(tool_error(
        "INVALID_REQUEST",
        "the arguments do not match the tool schema",
        false,
    ))
}

fn invalid_message(message: &'static str) -> Fail {
    Fail::Tool(tool_error("INVALID_REQUEST", message, false))
}

fn capability(message: &'static str) -> Fail {
    Fail::Tool(tool_error("CAPABILITY_UNAVAILABLE", message, false))
}

fn model_unavailable() -> Fail {
    Fail::Tool(tool_error("MODEL_UNAVAILABLE", "run gitcko setup", true))
}

fn internal(message: &'static str) -> Fail {
    Fail::Tool(tool_error("INTERNAL_ERROR", message, false))
}
