// SPDX-License-Identifier: AGPL-3.0-or-later

//! Remote acknowledgement and the stderr disclosure before the first model call
//! (PRIVACY §3, P5_PLAN §5.3).

use std::collections::BTreeMap;
use std::io::Write;

use gitcko_config::has_remote_acknowledgement;
use gitcko_model::{DestinationClass, ResolvedModels};
use gitcko_protocol::config::{ModelProvider, UserConfig};

use crate::Io;
use crate::config::{gitcko_paths, read_user_file, set_dotted, write_text};

/// `--no-input`, a closed stdin, or `CI=true`/`CI=1` never prompts.
pub fn prompts_disabled(
    no_input: bool,
    stdin_is_tty: bool,
    env: &BTreeMap<String, String>,
) -> bool {
    no_input || !stdin_is_tty || matches!(env.get("CI").map(String::as_str), Some("true" | "1"))
}

pub fn is_yes(text: &str) -> bool {
    matches!(text.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Record OpenRouter consent in the user file. The key is not written here.
pub fn record_openrouter_ack(env: &BTreeMap<String, String>) -> Result<(), String> {
    let paths = gitcko_paths(env).map_err(|err| err.to_string())?;
    let existing = read_user_file(&paths.config_file).map_err(ToString::to_string)?;
    let mut table = match &existing {
        Some(text) => {
            toml::from_str(text).map_err(|_| "could not read the user config".to_string())?
        }
        None => toml::Table::new(),
    };
    set_dotted(
        &mut table,
        "privacy.remote_acknowledged",
        toml::Value::Array(vec![toml::Value::String("openrouter".into())]),
    )
    .map_err(|()| "could not write the file".to_string())?;
    let text = toml::to_string(&table).map_err(|_| "could not write the file".to_string())?;
    write_text(&paths.config_file, &text, 0o644).map_err(ToString::to_string)
}

/// Remote binding without `privacy.remote_acknowledged` for that provider.
pub fn missing_remote_acknowledgement(
    user: &UserConfig,
    models: &ResolvedModels,
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

/// One stderr line before any provider call on a review run.
pub fn write_review_disclosure<O, E>(models: &ResolvedModels, io: &Io<O, E>)
where
    O: Write,
    E: Write,
{
    let mut disclosed = false;
    for (role, binding_name) in &models.roles {
        let Some(binding) = models.bindings.get(binding_name) else {
            continue;
        };
        if binding.destination.class != DestinationClass::CustomerRemote {
            continue;
        }
        disclosed = true;
        let provider = match binding.provider {
            ModelProvider::Openrouter => "openrouter",
            ModelProvider::Ollama => "ollama",
        };
        let _ = writeln!(
            lock(&io.stderr),
            "remote ({role:?}): selected code is sent to {provider} ({})",
            binding.model
        );
    }
    if !disclosed {
        let _ = writeln!(lock(&io.stderr), "local: code stays on this machine");
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}
