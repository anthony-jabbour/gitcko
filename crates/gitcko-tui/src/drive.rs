// SPDX-License-Identifier: AGPL-3.0-or-later

//! One review on a background thread. The thread calls `review_streaming` and stores
//! the outcome the same way `gitcko review` does. The screen only sees events.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use gitcko_config::{
    Ceilings, FolderConfig, GitckoPaths, RepoFiles, RunFlags, has_remote_acknowledgement,
    resolve_repo, resolve_user,
};
use gitcko_domain::{RepoPath, ReviewId};
use gitcko_git::Repo;
use gitcko_model::{DestinationClass, ModelProvider as DynProvider, resolve};
use gitcko_model_ollama::Ollama;
use gitcko_model_openai::OpenRouter;
use gitcko_protocol::config::{ModelProvider, ModelRole};
use gitcko_protocol::event::RunEvent;
use gitcko_protocol::review::Review;
use gitcko_review::{
    LOCAL_DEADLINE, NoFeedback, REMOTE_DEADLINE, ReviewInput, base_run, review_streaming,
};
use gitcko_storage::{ReviewStore, RunningMarker};
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::context;
use crate::progress::Phase;

pub(crate) enum DriveMsg {
    Event(Box<RunEvent>),
    Done { review: Box<Review>, stored: bool },
    Failed(String),
}

pub(crate) fn start(
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    cancel: CancellationToken,
) -> Receiver<DriveMsg> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let message = match run_review(&cwd, &env, &cancel, &tx) {
            Ok((review, stored)) => DriveMsg::Done {
                review: Box::new(review),
                stored,
            },
            Err(message) => DriveMsg::Failed(message),
        };
        let _ = tx.send(message);
    });
    rx
}

pub(crate) fn pump(app: &mut App, inbox: &mut Option<Receiver<DriveMsg>>) {
    let Some(rx) = inbox.as_ref() else {
        return;
    };
    loop {
        match rx.try_recv() {
            Ok(DriveMsg::Event(event)) => app.apply_event(&event),
            Ok(DriveMsg::Done { review, stored }) => {
                app.finish(*review, stored);
                *inbox = None;
                return;
            }
            Ok(DriveMsg::Failed(message)) => {
                app.fail_review(&message);
                *inbox = None;
                return;
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                if app.phase() == Phase::Running {
                    app.fail_review("the review stopped");
                }
                *inbox = None;
                return;
            }
        }
    }
}

fn run_review(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<DriveMsg>,
) -> Result<(Review, bool), String> {
    let repo = Repo::open(cwd).map_err(|_| "not a git repository".to_string())?;
    let (base, head) = comparison(&repo)?;
    reject_repo_models(cwd)?;
    let loaded = load_models(env)?;
    if let Some(provider) = loaded.unacked {
        return Err(format!("run `gitcko setup --accept-remote {provider}`"));
    }
    let paths = GitckoPaths::from_env(env).map_err(|err| err.to_string())?;
    let repo_id = context::repo_cache_id(cwd)
        .ok_or_else(|| "the repository path could not be resolved".to_string())?;
    let review_id = ReviewId::new().to_string();
    let store = ReviewStore::new(&paths.cache_dir);
    let base_sha = base.to_string();
    let head_sha = head.to_string();
    store
        .mark_running(
            &repo_id,
            &review_id,
            &RunningMarker {
                pid: std::process::id(),
                started: now_rfc3339(),
                head_sha: Some(head_sha),
                base_sha: Some(base_sha),
            },
        )
        .map_err(|_| "could not record that the review is running".to_string())?;
    let mut clear = Defer(Some({
        let root = paths.cache_dir.clone();
        let repo_id = repo_id.clone();
        let review_id = review_id.clone();
        move || {
            let _ = ReviewStore::new(root).clear_running(&repo_id, &review_id);
        }
    }));
    let outcome = execute(&repo, base, head, &loaded, &review_id, cancel, tx)?;
    let stored = store_outcome(&store, &repo_id, &outcome.review, &outcome.events);
    if let Some(clear) = clear.0.take() {
        clear();
    }
    Ok((outcome.review, stored))
}

struct Loaded {
    models: gitcko_model::ResolvedModels,
    unacked: Option<&'static str>,
    local_only: bool,
}

fn execute(
    repo: &Repo,
    base: gitcko_git::CommitId,
    head: gitcko_git::CommitId,
    loaded: &Loaded,
    review_id: &str,
    cancel: &CancellationToken,
    tx: &mpsc::Sender<DriveMsg>,
) -> Result<gitcko_review::Outcome, String> {
    let mut providers = BTreeMap::new();
    for (name, binding) in &loaded.models.bindings {
        let provider: Arc<dyn DynProvider> = match binding.provider {
            ModelProvider::Ollama => Arc::new(
                Ollama::new(binding.clone())
                    .map_err(|_| "could not use the ollama binding".to_string())?,
            ),
            ModelProvider::Openrouter => Arc::new(
                OpenRouter::new(binding.clone())
                    .map_err(|_| "could not use the openrouter binding".to_string())?,
            ),
        };
        providers.insert(name.clone(), provider);
    }
    let router = gitcko_model::Router::new(loaded.models.clone(), providers);
    let deadline = if loaded.local_only {
        LOCAL_DEADLINE
    } else {
        REMOTE_DEADLINE
    };
    let mut run = base_run(review_id.to_string(), Some(deadline));
    run.local_only = loaded.local_only;
    run.cancel = cancel.clone();
    let now = now_rfc3339;
    let input = ReviewInput {
        repo,
        base,
        head,
        now: &now,
        only_paths: &[],
        include_paths: &[],
    };
    let tx_events = tx.clone();
    let on_event = move |event: &RunEvent| {
        let _ = tx_events.send(DriveMsg::Event(Box::new(event.clone())));
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "could not start the runtime".to_string())?;
    Ok(runtime
        .block_on(async { review_streaming(input, &router, run, &NoFeedback, &on_event).await }))
}

fn comparison(repo: &Repo) -> Result<(gitcko_git::CommitId, gitcko_git::CommitId), String> {
    let head = repo
        .resolve("HEAD")
        .map_err(|_| "could not resolve the head ref".to_string())?;
    let mut base_tip = None;
    for candidate in ["main", "master", "origin/HEAD"] {
        if let Ok(id) = repo.resolve(candidate) {
            base_tip = Some(id);
            break;
        }
    }
    let Some(base_tip) = base_tip else {
        return Err("could not resolve the base ref".to_string());
    };
    repo.merge_base(base_tip, head)
        .map_err(|_| "could not find a merge base".to_string())?
        .ok_or_else(|| "could not find a merge base".to_string())
        .map(|base| (base, head))
}

fn load_models(env: &BTreeMap<String, String>) -> Result<Loaded, String> {
    let paths = GitckoPaths::from_env(env).map_err(|err| err.to_string())?;
    let text = match fs::read_to_string(&paths.config_file) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("could not read the user config".to_string()),
    };
    let resolved = resolve_user(text.as_deref(), env, &RunFlags::default())
        .map_err(|_| "could not read the user config".to_string())?;
    let user = resolved.user_config();
    let models = user.models.clone().unwrap_or_default();
    let models = resolve(&models, env)
        .map_err(|_| "No model configured. Run `gitcko setup`.".to_string())?;
    if models
        .require(&[ModelRole::Review, ModelRole::Judge])
        .is_err()
    {
        return Err("No model configured. Run `gitcko setup`.".to_string());
    }
    let unacked = unacknowledged(&user, &models);
    let local_only = models
        .bindings
        .values()
        .all(|binding| binding.destination.class == DestinationClass::OnDevice);
    Ok(Loaded {
        models,
        unacked,
        local_only,
    })
}

fn unacknowledged(
    user: &gitcko_protocol::config::UserConfig,
    models: &gitcko_model::ResolvedModels,
) -> Option<&'static str> {
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

fn reject_repo_models(cwd: &Path) -> Result<(), String> {
    let path = cwd.join(".gitcko").join("config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("could not read repository config".to_string()),
    };
    let files = RepoFiles {
        folders: vec![FolderConfig {
            folder: None,
            config_toml: Some(text),
            rules_md: None,
        }],
        legacy_root_file: false,
    };
    let reviewed = RepoPath::parse("README").map_err(|_| "could not read repository config")?;
    resolve_repo(&files, &reviewed, &RunFlags::default(), &Ceilings::local())
        .map(|_| ())
        .map_err(|_| "repository config cannot set models".to_string())
}

pub(crate) fn store_outcome(
    store: &ReviewStore,
    repo_id: &str,
    review: &Review,
    events: &[RunEvent],
) -> bool {
    if store.put(repo_id, review).is_err() {
        return false;
    }
    if events.is_empty() {
        return true;
    }
    let path = store
        .root()
        .join("repos")
        .join(repo_id)
        .join("reviews")
        .join(format!("{}.events", review.review_id));
    if let Ok(bytes) = serde_json::to_vec(events) {
        let _ = fs::write(path, bytes);
    }
    true
}

struct Defer<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for Defer<F> {
    fn drop(&mut self) {
        if let Some(action) = self.0.take() {
            action();
        }
    }
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    unix_to_rfc3339(secs)
}

fn unix_to_rfc3339(secs: u64) -> String {
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let hour = tod / 3_600;
    let minute = (tod % 3_600) / 60;
    let second = tod % 60;
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitcko_storage::{RunState, StoreError};

    #[test]
    fn a_finished_review_is_stored_and_the_marker_does_not_linger() {
        let review: Review = serde_json::from_str(include_str!(
            "../../gitcko-protocol/tests/data/review-full.json"
        ))
        .expect("fixture review");
        let dir = gitcko_test_support::TempDir::new("tui-store");
        let store = ReviewStore::new(dir.path());
        assert!(
            matches!(
                store.get("repo", &review.review_id),
                Err(StoreError::NotFound)
            ),
            "nothing is stored before the run finishes"
        );
        store
            .mark_running(
                "repo",
                &review.review_id,
                &RunningMarker {
                    pid: std::process::id(),
                    started: "2026-01-01T00:00:00Z".into(),
                    head_sha: Some(review.target.head_sha.clone()),
                    base_sha: Some(review.target.base_sha.clone()),
                },
            )
            .expect("marker");
        assert!(
            matches!(
                store.run_state("repo", &review.review_id).expect("state"),
                RunState::Running(_)
            ),
            "the live pid is running"
        );
        assert!(store_outcome(&store, "repo", &review, &[]));
        store
            .clear_running("repo", &review.review_id)
            .expect("clear");
        let loaded = store.get("repo", &review.review_id).expect("stored review");
        assert_eq!(loaded.review_id, review.review_id);
        assert_eq!(loaded.status, review.status);
        assert!(
            matches!(
                store.run_state("repo", &review.review_id).expect("state"),
                RunState::Idle
            ),
            "clearing the marker leaves the run idle"
        );
    }

    #[test]
    fn the_epoch_formats_as_utc() {
        assert_eq!(unix_to_rfc3339(0), "1970-01-01T00:00:00Z");
    }
}
