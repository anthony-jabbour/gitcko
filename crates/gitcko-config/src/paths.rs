// SPDX-License-Identifier: AGPL-3.0-or-later

//! Config, cache, credentials and log paths from an env map (LOCAL_DEPLOYMENT §5).

use std::collections::BTreeMap;
#[cfg(not(windows))]
use std::path::Path;
use std::path::PathBuf;

/// Config, credentials, cache and log locations (LOCAL_DEPLOYMENT §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitckoPaths {
    /// User `config.toml`.
    pub config_file: PathBuf,
    /// Fallback `credentials.toml` next to the default config dir, not next to an override.
    pub credentials_file: PathBuf,
    /// Index and review history.
    pub cache_dir: PathBuf,
    /// Rotated logs; never source text.
    pub logs_dir: PathBuf,
}

/// Why a path couldn't be resolved. Names the variable, never its value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// Unix: neither `HOME` nor `XDG_*` when those paths are needed.
    #[error("HOME is not set")]
    NoHome,
    /// Windows: `%APPDATA%` missing.
    #[error("APPDATA is not set")]
    NoAppData,
    /// Windows: `%LOCALAPPDATA%` missing.
    #[error("LOCALAPPDATA is not set")]
    NoLocalAppData,
    /// `GITCKO_CONFIG` or `GITCKO_CACHE_DIR` was relative.
    #[error("{var} must be an absolute path")]
    NotAbsolute {
        /// The override variable (`GITCKO_CONFIG` or `GITCKO_CACHE_DIR`).
        var: &'static str,
    },
}

impl GitckoPaths {
    /// Resolve from an explicit env map so tests never touch the process environment.
    pub fn from_env(env: &BTreeMap<String, String>) -> Result<GitckoPaths, PathError> {
        #[cfg(windows)]
        {
            from_env_windows(env)
        }
        #[cfg(not(windows))]
        {
            from_env_unix(env)
        }
    }
}

fn nonempty<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(String::as_str).filter(|s| !s.is_empty())
}

fn absolute(var: &'static str, value: &str) -> Result<PathBuf, PathError> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(PathError::NotAbsolute { var })
    }
}

fn config_override(env: &BTreeMap<String, String>) -> Result<Option<PathBuf>, PathError> {
    nonempty(env, "GITCKO_CONFIG")
        .map(|v| absolute("GITCKO_CONFIG", v))
        .transpose()
}

fn cache_override(env: &BTreeMap<String, String>) -> Result<Option<PathBuf>, PathError> {
    nonempty(env, "GITCKO_CACHE_DIR")
        .map(|v| absolute("GITCKO_CACHE_DIR", v))
        .transpose()
}

#[cfg(not(windows))]
fn from_env_unix(env: &BTreeMap<String, String>) -> Result<GitckoPaths, PathError> {
    let home = nonempty(env, "HOME").map(Path::new);
    let config_dir = match nonempty(env, "XDG_CONFIG_HOME") {
        Some(xdg) => Some(PathBuf::from(xdg).join("gitcko")),
        None => home.map(|h| h.join(".config").join("gitcko")),
    };
    let config_file = match config_override(env)? {
        Some(path) => path,
        None => config_dir
            .as_ref()
            .ok_or(PathError::NoHome)?
            .join("config.toml"),
    };
    let credentials_file = match &config_dir {
        Some(dir) => dir.join("credentials.toml"),
        None => config_file.with_file_name("credentials.toml"),
    };
    let cache_dir = match cache_override(env)? {
        Some(path) => path,
        None => match nonempty(env, "XDG_CACHE_HOME") {
            Some(xdg) => PathBuf::from(xdg).join("gitcko"),
            None => home.ok_or(PathError::NoHome)?.join(".cache").join("gitcko"),
        },
    };
    let logs_dir = match nonempty(env, "XDG_STATE_HOME") {
        Some(xdg) => PathBuf::from(xdg).join("gitcko").join("logs"),
        None => home
            .ok_or(PathError::NoHome)?
            .join(".local")
            .join("state")
            .join("gitcko")
            .join("logs"),
    };
    Ok(GitckoPaths {
        config_file,
        credentials_file,
        cache_dir,
        logs_dir,
    })
}

#[cfg(windows)]
fn from_env_windows(env: &BTreeMap<String, String>) -> Result<GitckoPaths, PathError> {
    let appdata = nonempty(env, "APPDATA").ok_or(PathError::NoAppData)?;
    let local = nonempty(env, "LOCALAPPDATA").ok_or(PathError::NoLocalAppData)?;
    let config_dir = PathBuf::from(appdata).join("gitcko");
    let config_file = match config_override(env)? {
        Some(path) => path,
        None => config_dir.join("config.toml"),
    };
    let cache_dir = match cache_override(env)? {
        Some(path) => path,
        None => PathBuf::from(local).join("gitcko").join("cache"),
    };
    Ok(GitckoPaths {
        credentials_file: config_dir.join("credentials.toml"),
        config_file,
        cache_dir,
        logs_dir: PathBuf::from(local).join("gitcko").join("logs"),
    })
}
