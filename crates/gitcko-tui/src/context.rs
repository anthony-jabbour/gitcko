// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the shell shows before anyone presses a key: the checkout, the index and the
//! configured model. Failures become a status line. They never include a secret or a
//! path from inside the repository.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use gitcko_config::{GitckoPaths, RunFlags, has_remote_acknowledgement, resolve_user};
use gitcko_git::Repo;
use gitcko_index::{Store, StoreError};
use gitcko_model::{DestinationClass, ResolveError, resolve};
use gitcko_protocol::config::{ModelProvider, ModelRole, ModelsConfig};
use sha2::{Digest, Sha256};

use crate::app::App;
use crate::header::{Header, IndexState, Locality};
use crate::theme::{self};

/// SHA-256 of the canonical working directory, hex. Matches the CLI's cache key.
pub fn repo_cache_id(cwd: &Path) -> Option<String> {
    let canonical = cwd.canonicalize().ok()?;
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    Some(hex_encode(&hasher.finalize()))
}

pub fn assemble(cwd: &Path, env: &BTreeMap<String, String>) -> App {
    let mut app = App::preview();
    app.color = false;
    app.header = Header {
        repo: repo_label(cwd),
        branch: "—".into(),
        base: "—".into(),
        index: IndexState::Missing,
        model: "none".into(),
        locality: Locality::Local,
    };
    let preference = model_and_color(env, &mut app);
    app.color = theme::color_enabled(env, preference);
    app.reduced_motion = theme::reduced_motion(env);
    match Repo::open(cwd) {
        Ok(repo) => fill_repo(&mut app, cwd, env, &repo),
        Err(_) => {
            app.no_repo = Some(cwd.display().to_string());
            app.header.repo = cwd.display().to_string();
        }
    }
    app
}

fn fill_repo(app: &mut App, cwd: &Path, env: &BTreeMap<String, String>, repo: &Repo) {
    let (branch, head) = head_label(cwd, repo);
    app.header.branch = branch;
    app.header.base = base_name(repo);
    app.header.index = index_state(env, repo, cwd, head.as_deref());
    if app.status.is_empty()
        && let Some(banner) = app.header.index.banner()
    {
        app.status = banner;
    }
}

fn model_and_color(env: &BTreeMap<String, String>, app: &mut App) -> Option<bool> {
    let paths = match GitckoPaths::from_env(env) {
        Ok(paths) => paths,
        Err(err) => {
            app.status = err.to_string();
            return None;
        }
    };
    let text = match fs::read_to_string(&paths.config_file) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => {
            app.status = "could not read the user config".into();
            return None;
        }
    };
    let resolved = match resolve_user(text.as_deref(), env, &RunFlags::default()) {
        Ok(resolved) => resolved,
        Err(_) => {
            app.status = "could not read the user config".into();
            return None;
        }
    };
    let user = resolved.user_config();
    let preference = user.color;
    let models = user.models.clone().unwrap_or_default();
    match resolve(&models, env) {
        Ok(resolved_models) => {
            let Some(binding) = resolved_models.binding_for(ModelRole::Review) else {
                app.status = "No model configured. Run `gitcko setup`.".into();
                return preference;
            };
            app.header.model = binding.model.clone();
            app.header.locality = match binding.destination.class {
                DestinationClass::OnDevice => Locality::Local,
                DestinationClass::CustomerRemote => Locality::Remote {
                    provider: provider_name(binding.provider).into(),
                },
            };
            if binding.destination.class == DestinationClass::CustomerRemote
                && binding.provider == ModelProvider::Openrouter
                && !has_remote_acknowledgement(&user, binding.provider)
            {
                app.status = "run `gitcko setup --accept-remote openrouter`".into();
            }
        }
        Err(ResolveError::MissingBinding(_)) => {
            app.status = "No model configured. Run `gitcko setup`.".into();
        }
        Err(ResolveError::MissingCredential { provider, .. }) => {
            apply_configured_binding(&models, app);
            app.header.locality = Locality::Remote {
                provider: provider.to_string(),
            };
            app.status = format!("provider: {provider} auth failed — run `gitcko doctor`");
        }
        Err(ResolveError::UnknownBinding { .. }) => {
            app.status = "a model binding is not defined — run `gitcko doctor`".into();
        }
        Err(ResolveError::BadEndpoint(_)) => {
            app.status = "the model endpoint is not usable — run `gitcko doctor`".into();
        }
    }
    preference
}

fn apply_configured_binding(models: &ModelsConfig, app: &mut App) {
    let Some(bindings) = models.bindings.as_ref() else {
        return;
    };
    let name = models
        .roles
        .as_ref()
        .and_then(|roles| roles.get(&ModelRole::Review))
        .cloned()
        .or_else(|| {
            // Unset role uses the binding named after its class, which is `smart`.
            bindings.contains_key("smart").then(|| "smart".to_string())
        });
    let Some(name) = name else {
        return;
    };
    let Some(binding) = bindings.get(&name) else {
        return;
    };
    app.header.model = binding.model.clone();
    app.header.locality = match binding.provider {
        ModelProvider::Ollama => Locality::Local,
        ModelProvider::Openrouter => Locality::Remote {
            provider: "openrouter".into(),
        },
    };
}

fn provider_name(provider: ModelProvider) -> &'static str {
    match provider {
        ModelProvider::Ollama => "ollama",
        ModelProvider::Openrouter => "openrouter",
    }
}

fn index_state(
    env: &BTreeMap<String, String>,
    repo: &Repo,
    cwd: &Path,
    head: Option<&str>,
) -> IndexState {
    let paths = match GitckoPaths::from_env(env) {
        Ok(paths) => paths,
        Err(err) => {
            return IndexState::Failed {
                reason: err.to_string(),
            };
        }
    };
    let Some(repo_id) = repo_cache_id(cwd) else {
        return IndexState::Failed {
            reason: "the repository path could not be resolved".into(),
        };
    };
    let store = Store::new(&paths.cache_dir);
    match store.active(&repo_id) {
        Ok(None) => IndexState::Missing,
        Err(StoreError::Corrupt { .. }) => IndexState::Failed {
            reason: "the index is corrupt".into(),
        },
        Err(_) => IndexState::Failed {
            reason: "the index could not be read".into(),
        },
        Ok(Some(id)) => match head {
            None => IndexState::Stale,
            Some(head) => match store.open(repo, &repo_id, &id) {
                Ok(revision) if revision.commit == head => IndexState::Ready,
                Ok(_) => IndexState::Stale,
                Err(StoreError::Corrupt { .. }) => IndexState::Failed {
                    reason: "the index is corrupt".into(),
                },
                Err(_) => IndexState::Failed {
                    reason: "the index could not be read".into(),
                },
            },
        },
    }
}

fn repo_label(cwd: &Path) -> String {
    origin_slug(cwd).unwrap_or_else(|| folder_name(cwd))
}

fn folder_name(cwd: &Path) -> String {
    cwd.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("repository")
        .to_string()
}

fn origin_slug(cwd: &Path) -> Option<String> {
    let git = git_dir(cwd)?;
    let text = fs::read_to_string(git.join("config")).ok()?;
    let mut in_origin = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_origin = trimmed == "[remote \"origin\"]";
            continue;
        }
        if !in_origin {
            continue;
        }
        let Some(url) = trimmed.strip_prefix("url") else {
            continue;
        };
        let url = url.trim().strip_prefix('=')?.trim();
        if let Some(slug) = slug_from_url(url) {
            return Some(slug);
        }
    }
    None
}

fn slug_from_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(".git");
    let path = if let Some(rest) = url.strip_prefix("git@") {
        rest.split_once(':')?.1
    } else {
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .or_else(|| url.strip_prefix("ssh://"))?;
        rest.split_once('/')?.1
    };
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    Some(format!("{owner}/{repo}"))
}

fn head_label(cwd: &Path, repo: &Repo) -> (String, Option<String>) {
    let sha = repo.resolve("HEAD").ok().map(|id| id.to_string());
    let branch = current_branch(cwd).unwrap_or_else(|| match &sha {
        Some(sha) => short_sha(sha),
        None => "—".into(),
    });
    (branch, sha)
}

fn current_branch(cwd: &Path) -> Option<String> {
    let git = git_dir(cwd)?;
    let head = fs::read_to_string(git.join("HEAD")).ok()?;
    let name = head
        .lines()
        .next()?
        .trim()
        .strip_prefix("ref: refs/heads/")?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn base_name(repo: &Repo) -> String {
    for candidate in ["main", "master", "origin/HEAD"] {
        if repo.resolve(candidate).is_ok() {
            return candidate.to_string();
        }
    }
    "HEAD".into()
}

fn git_dir(cwd: &Path) -> Option<std::path::PathBuf> {
    let git = cwd.join(".git");
    if git.is_dir() {
        return Some(git);
    }
    let text = fs::read_to_string(&git).ok()?;
    let rel = text.trim().strip_prefix("gitdir:")?.trim();
    let path = std::path::PathBuf::from(rel);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(cwd.join(path))
    }
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
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
