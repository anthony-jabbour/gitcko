// SPDX-License-Identifier: AGPL-3.0-or-later

//! Provider keys: env, then OS keychain, then a mode-`0600` file.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;

use gitcko_protocol::config::ModelProvider;

/// Where a key was found. Never the key itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSource {
    /// `OPENROUTER_API_KEY` (and later Cloud vars).
    Env,
    /// OS keychain, service `gitcko`.
    Keychain,
    /// `credentials.toml` mode `0600`.
    File,
}

/// A provider key. `Debug` is always `<secret> (source)`, never the bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret {
    source: SecretSource,
    value: String,
}

impl Secret {
    /// The raw key, for the router only.
    pub fn expose(&self) -> &str {
        &self.value
    }

    /// Where it was loaded from (`config` and `doctor` print this).
    pub fn source(&self) -> SecretSource {
        self.source
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<secret> ({:?})", self.source)
    }
}

/// Why credentials couldn't be loaded. Never a path, key value, or file body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredError {
    /// Unix mode allows group or other to read the file.
    #[error("credentials file is readable by the group or others")]
    TooOpen,
    /// TOML is missing, not a table, or a field has the wrong type.
    #[error("the credentials file is not valid")]
    Invalid,
    /// A key other than `providers.openrouter.api_key`.
    #[error("unknown key in the credentials file ({key})")]
    UnknownKey {
        /// Dotted path of the unexpected key, never its value.
        key: String,
    },
    /// A filesystem call failed. `op` is a fixed verb, never a path.
    #[error("the credentials file couldn't {op}")]
    Io {
        /// `read` or `stat`.
        op: &'static str,
    },
    /// The OS store returned an error other than a missing entry.
    #[error("couldn't read the keychain")]
    Keychain,
}

/// Parsed `[providers.openrouter] api_key` (CONFIGURATION §10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CredentialsFile {
    keys: BTreeMap<String, String>,
}

impl CredentialsFile {
    /// Parse TOML text. Callers that read from disk should use [`load_file`].
    pub fn parse(text: &str) -> Result<CredentialsFile, CredError> {
        let value: toml::Value = toml::from_str(text).map_err(|_| CredError::Invalid)?;
        let table = value.as_table().ok_or(CredError::Invalid)?;
        let mut keys = BTreeMap::new();
        for (top, inner) in table {
            if top != "providers" {
                return Err(CredError::UnknownKey { key: top.clone() });
            }
            let providers = inner.as_table().ok_or(CredError::Invalid)?;
            for (name, body) in providers {
                if name != "openrouter" {
                    return Err(CredError::UnknownKey {
                        key: format!("providers.{name}"),
                    });
                }
                let body = body.as_table().ok_or(CredError::Invalid)?;
                for (field, val) in body {
                    if field != "api_key" {
                        return Err(CredError::UnknownKey {
                            key: format!("providers.{name}.{field}"),
                        });
                    }
                    let secret = val.as_str().ok_or(CredError::Invalid)?;
                    if !secret.is_empty() {
                        keys.insert(name.clone(), secret.to_string());
                    }
                }
            }
        }
        Ok(CredentialsFile { keys })
    }

    /// The api_key for a provider name (`openrouter`), if present and non-empty.
    pub fn get(&self, provider: &str) -> Option<&str> {
        self.keys.get(provider).map(String::as_str)
    }
}

/// Read `credentials.toml`, refusing a Unix file that is group- or world-readable.
pub fn load_file(path: &Path) -> Result<CredentialsFile, CredError> {
    let meta = fs::metadata(path).map_err(|_| CredError::Io { op: "stat" })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(CredError::TooOpen);
        }
    }
    let _ = meta;
    let text = fs::read_to_string(path).map_err(|_| CredError::Io { op: "read" })?;
    CredentialsFile::parse(&text)
}

/// Env, then keychain, then the fallback file. Empty values are skipped.
pub fn resolve_key(
    provider: ModelProvider,
    env: &BTreeMap<String, String>,
    keychain: &dyn Keychain,
    file: Option<&CredentialsFile>,
) -> Result<Option<Secret>, CredError> {
    let Some(var) = env_var(provider) else {
        return Ok(None);
    };
    if let Some(value) = env.get(var).filter(|v| !v.is_empty()) {
        return Ok(Some(Secret {
            source: SecretSource::Env,
            value: value.clone(),
        }));
    }
    let account = account(provider);
    if let Some(value) = keychain.get(account)?.filter(|v| !v.is_empty()) {
        return Ok(Some(Secret {
            source: SecretSource::Keychain,
            value,
        }));
    }
    if let Some(value) = file.and_then(|f| f.get(account)).filter(|v| !v.is_empty()) {
        return Ok(Some(Secret {
            source: SecretSource::File,
            value: value.to_string(),
        }));
    }
    Ok(None)
}

fn env_var(provider: ModelProvider) -> Option<&'static str> {
    match provider {
        ModelProvider::Openrouter => Some("OPENROUTER_API_KEY"),
        ModelProvider::Ollama => None,
    }
}

fn account(provider: ModelProvider) -> &'static str {
    match provider {
        ModelProvider::Openrouter => "openrouter",
        ModelProvider::Ollama => "ollama",
    }
}

/// Looks up `gitcko` / `<provider>` in an OS or test store.
pub trait Keychain {
    /// `None` when the account has no entry.
    fn get(&self, account: &str) -> Result<Option<String>, CredError>;
}

/// In-memory store for tests. The real OS store is [`OsKeychain`].
#[derive(Default)]
pub struct MemoryKeychain {
    /// account → secret.
    pub entries: BTreeMap<String, String>,
}

impl Keychain for MemoryKeychain {
    fn get(&self, account: &str) -> Result<Option<String>, CredError> {
        Ok(self.entries.get(account).cloned())
    }
}

/// macOS Keychain, Windows Credential Manager, Linux Secret Service.
pub struct OsKeychain;

impl Keychain for OsKeychain {
    fn get(&self, account: &str) -> Result<Option<String>, CredError> {
        let entry = keyring::Entry::new("gitcko", account).map_err(|_| CredError::Keychain)?;
        match entry.get_password() {
            Ok(password) if password.is_empty() => Ok(None),
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(CredError::Keychain),
        }
    }
}
