// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading one config file: parse with a line for every key, check it (CONFIGURATION
//! §11), and turn it into `(key, value, source)` items for the merge.

use std::collections::BTreeMap;

use gitcko_protocol::config::{ModelProvider, ModelsConfig, RepoConfig, UserConfig};
use gitcko_protocol::review::Tier;
use serde_json::Value;
use toml::de::{DeTable, DeValue};

use crate::{ConfigError, Layer, Source};

/// How the user file is named in messages: never the real local path.
pub(crate) const USER_FILE: &str = "~/.config/gitcko/config.toml";

/// One loaded file, ready to merge.
pub(crate) type Items = Vec<(String, Value, Source)>;

fn line_of(text: &str, offset: usize) -> u32 {
    (text[..offset.min(text.len())].matches('\n').count() + 1) as u32
}

/// The line of every key, by dotted path; items of an array of tables are `key[i]`
/// (`rules[0]`, `rules[0].scope`).
fn lines(text: &str, table: &DeTable<'_>, prefix: &str, out: &mut BTreeMap<String, u32>) {
    for (key, value) in table.iter() {
        let name: &str = key.get_ref();
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}.{name}")
        };
        out.insert(path.clone(), line_of(text, key.span().start));
        match value.get_ref() {
            DeValue::Table(inner) => lines(text, inner, &path, out),
            DeValue::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    if let DeValue::Table(inner) = item.get_ref() {
                        let item_path = format!("{path}[{i}]");
                        out.insert(item_path.clone(), line_of(text, item.span().start));
                        lines(text, inner, &item_path, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Leaf values by dotted path. Arrays are leaves; tables recurse.
pub(crate) fn flatten(value: &Value) -> Vec<(String, Value)> {
    fn walk(prefix: &str, value: &Value, out: &mut Vec<(String, Value)>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    walk(&path, inner, out);
                }
            }
            leaf => out.push((prefix.to_string(), leaf.clone())),
        }
    }
    let mut out = Vec::new();
    walk("", value, &mut out);
    out
}

/// A parsed file: its JSON form and the line of every key.
struct Parsed {
    label: String,
    json: Value,
    lines: BTreeMap<String, u32>,
}

impl Parsed {
    fn error(&self, key: Option<&str>, message: impl Into<String>) -> ConfigError {
        // Error keys drop list positions (`rules.scope`); lines keep them.
        let line = key.and_then(|k| self.lines.get(k).copied());
        ConfigError {
            file: self.label.clone(),
            line,
            key: key.map(strip_indices),
            message: message.into(),
        }
    }

    fn source(&self, layer: Layer, key: &str) -> Source {
        Source {
            layer,
            file: Some(self.label.clone()),
            line: self.lines.get(key).copied(),
        }
    }
}

fn strip_indices(key: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for c in key.chars() {
        match c {
            '[' => skipping = true,
            ']' => skipping = false,
            c if !skipping => out.push(c),
            _ => {}
        }
    }
    out
}

fn parse(label: &str, text: &str) -> Result<Parsed, ConfigError> {
    let syntax = |e: toml::de::Error| ConfigError {
        file: label.to_string(),
        line: e.span().map(|s| line_of(text, s.start)),
        key: None,
        // The message only: the formatted error quotes the source line, which could
        // hold a secret.
        message: e.message().to_string(),
    };
    let table = DeTable::parse(text).map_err(syntax)?;
    let mut key_lines = BTreeMap::new();
    lines(text, table.get_ref(), "", &mut key_lines);
    let json: Value = toml::from_str(text).map_err(syntax)?;
    let parsed = Parsed {
        label: label.to_string(),
        json,
        lines: key_lines,
    };
    reject_secrets(&parsed, "", &parsed.json)?;
    Ok(parsed)
}

/// A value that looks like a credential is an error naming the key, never echoing the
/// value (CONFIGURATION §10). Secrets live in the keychain or env vars.
fn reject_secrets(parsed: &Parsed, path: &str, value: &Value) -> Result<(), ConfigError> {
    let redactor = gitcko_redact::Redactor::new();
    match value {
        Value::String(s) if redactor.redact(s) != s.as_str() => Err(parsed.error(
            Some(path),
            "looks like a secret; keep secrets in the keychain or environment, never in config files",
        )),
        Value::Object(map) => map.iter().try_for_each(|(k, v)| {
            let child = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
            reject_secrets(parsed, &child, v)
        }),
        Value::Array(items) => items.iter().enumerate().try_for_each(|(i, v)| {
            let child = if matches!(v, Value::Object(_)) { format!("{path}[{i}]") } else { path.to_string() };
            reject_secrets(parsed, &child, v)
        }),
        _ => Ok(()),
    }
}

/// Typed check against the public type: unknown keys, wrong types, bad enum values.
fn typed<T: serde::de::DeserializeOwned>(parsed: &Parsed, text: &str) -> Result<T, ConfigError> {
    toml::from_str(text).map_err(|e: toml::de::Error| {
        let line = e.span().map(|s| line_of(text, s.start));
        // The key is the one on the error's line (the deepest, if a line has several),
        // never a word taken from the message, which can quote the value.
        let key = parsed
            .lines
            .iter()
            .filter(|(_, l)| Some(**l) == line)
            .map(|(k, _)| k.clone())
            .max_by_key(|k| k.len());
        ConfigError {
            file: parsed.label.clone(),
            line,
            key: key.map(|k| strip_indices(&k)),
            message: value_free(e.message()),
        }
    })
}

/// serde's message without the value it quotes ("invalid type: string \"x\"" could be
/// anything the file holds). What was expected comes from our own types, so it's kept.
pub(crate) fn value_free(message: &str) -> String {
    let expected = message.split_once(", expected ").map(|(_, e)| e);
    if message.starts_with("unknown field") {
        "unknown key".into()
    } else if message.starts_with("missing field") {
        message.to_string()
    } else if let Some(expected) = expected {
        if message.starts_with("unknown variant") {
            format!("not an allowed value; expected {expected}")
        } else {
            format!("wrong type or value; expected {expected}")
        }
    } else {
        "invalid value".into()
    }
}

/// Limits the types can't express, shared by files and run flags.
pub(crate) fn check_repo_values(config: &RepoConfig) -> Result<(), (&'static str, &'static str)> {
    if config.strictness.is_some_and(|s| !(1..=3).contains(&s)) {
        return Err(("strictness", "must be 1, 2 or 3"));
    }
    if config
        .instructions
        .as_ref()
        .is_some_and(|i| i.chars().count() > 4000)
    {
        return Err(("instructions", "longer than 4,000 characters"));
    }
    Ok(())
}

/// Limits for user settings, shared by the user file and run flags.
pub(crate) fn check_user_values(config: &UserConfig) -> Result<(), (&'static str, &'static str)> {
    if let Some(review) = &config.review {
        if review.context.is_some_and(|c| c > 60) {
            return Err(("review.context", "must be 0 to 60"));
        }
        if review.width.is_some_and(|w| !(40..=240).contains(&w)) {
            return Err(("review.width", "must be 40 to 240"));
        }
    }
    Ok(())
}

/// `[models]` limits the types can't express (MODEL_ROUTING §3). Like every config
/// error, these name the key but never echo the value.
fn check_models(models: &ModelsConfig) -> Result<(), (String, &'static str)> {
    let bindings = models.bindings.as_ref();
    for (name, binding) in bindings.into_iter().flatten() {
        let key = |field: &str| format!("models.bindings.{name}.{field}");
        // Settings are rebuilt from dotted keys, so a dot in a name would split it.
        let plain = !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !plain {
            return Err((
                "models.bindings".into(),
                "binding names use only a-z, 0-9, _ and -",
            ));
        }
        if !exact_model(binding.provider, &binding.model) {
            return Err((
                key("model"),
                "must be an exact model id (provider/model on OpenRouter, name:tag on Ollama), not a moving alias",
            ));
        }
        let ollama = binding.provider == ModelProvider::Ollama;
        if ollama && binding.data_policy.is_some() {
            return Err((key("data_policy"), "is only for openrouter bindings"));
        }
        if !ollama && binding.endpoint.is_some() {
            return Err((key("endpoint"), "is only for ollama bindings"));
        }
        if !ollama && binding.context.is_some() {
            return Err((key("context"), "is only for ollama bindings"));
        }
        if binding
            .endpoint
            .as_deref()
            .is_some_and(|e| !valid_origin(e))
        {
            return Err((
                key("endpoint"),
                "must be an https:// origin with no path (plain http only for localhost)",
            ));
        }
        if binding
            .context
            .is_some_and(|c| !(2048..=1_048_576).contains(&c))
        {
            return Err((key("context"), "must be 2048 to 1048576"));
        }
    }
    let defined = |name: &String| bindings.is_some_and(|b| b.contains_key(name));
    for (role, name) in models.roles.iter().flatten() {
        if !defined(name) {
            return Err((
                format!("models.roles.{}", wire_name(role)),
                "names a binding that isn't defined",
            ));
        }
    }
    for (class, names) in models.fallbacks.iter().flatten() {
        if !names.iter().all(defined) {
            return Err((
                format!("models.fallbacks.{}", wire_name(class)),
                "names a binding that isn't defined",
            ));
        }
    }
    Ok(())
}

/// The serde name of a key enum, so errors use the spelling the user wrote.
fn wire_name(key: &impl serde::Serialize) -> String {
    serde_json::to_value(key)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Exact ids only: OpenRouter `vendor/model` (with an optional variant such as
/// `:free`), Ollama `name:tag`. `latest`, untagged Ollama names, OpenRouter's own
/// routers (`openrouter/auto`) and `~` aliases all move without notice.
fn exact_model(provider: ModelProvider, model: &str) -> bool {
    let printable = !model.is_empty() && model.bytes().all(|b| b.is_ascii_graphic());
    let tag_moves = |tag: &str| tag.is_empty() || tag == "latest";
    printable
        && match provider {
            ModelProvider::Ollama => model.rsplit_once(':').is_some_and(|(name, tag)| {
                !name.is_empty() && !tag_moves(tag) && !tag.contains('/')
            }),
            ModelProvider::Openrouter => {
                let (id, variant) = model.split_once(':').unwrap_or((model, "x"));
                id.split_once('/').is_some_and(|(vendor, name)| {
                    !vendor.is_empty() && !name.is_empty() && vendor != "openrouter"
                }) && !model.starts_with('~')
                    && !tag_moves(variant)
            }
        }
}

/// A path or glob in a repo file must be a plain repository path relative to its
/// folder: everything `RepoPath` requires (no `..`, empty parts, control characters or
/// `.git`), plus no `\\`, no leading `~` and no drive letter, so it can't name a file
/// outside the folder on any OS. Glob characters are allowed.
fn escapes(path: &str) -> bool {
    gitcko_domain::RepoPath::parse(path).is_err()
        || path.contains('\\')
        || path.starts_with('~')
        || path.chars().nth(1) == Some(':')
}

pub(crate) fn load_repo(
    label: &str,
    text: &str,
    is_root: bool,
    max_tier: Tier,
) -> Result<Items, ConfigError> {
    let parsed = parse(label, text)?;
    if parsed.json.get("models").is_some() {
        return Err(parsed.error(
            Some("models"),
            "models can't be set in repository config: repository files are untrusted (CONFIGURATION §2)",
        ));
    }
    let config: RepoConfig = typed(&parsed, text)?;
    match config.schema_version {
        None if is_root => {
            return Err(parsed.error(
                None,
                "schema_version is required in the root config; add `schema_version = 1`",
            ));
        }
        Some(v) if v != 1 => {
            return Err(parsed.error(
                Some("schema_version"),
                format!("unknown version {v}; this Gitcko reads 1"),
            ));
        }
        _ => {}
    }
    check_repo_values(&config).map_err(|(key, message)| parsed.error(Some(key), message))?;
    // Checked per file, so a nested config above the ceiling falls back to its parent
    // like any other invalid nested config.
    if config
        .tier
        .is_some_and(|t| crate::tier_rank(t) > crate::tier_rank(max_tier))
    {
        return Err(parsed.error(Some("tier"), crate::above_ceiling(max_tier)));
    }
    // Every path and glob, with the key it lives under.
    let mut paths: Vec<(String, &String)> = Vec::new();
    for (i, rule) in config.rules.iter().flatten().enumerate() {
        paths.extend(
            rule.scope
                .iter()
                .flatten()
                .map(|p| (format!("rules[{i}].scope"), p)),
        );
    }
    for (i, file) in config
        .context
        .iter()
        .flat_map(|c| c.files.iter().flatten())
        .enumerate()
    {
        paths.push((format!("context.files[{i}].path"), &file.path));
        paths.extend(
            file.scope
                .iter()
                .flatten()
                .map(|p| (format!("context.files[{i}].scope"), p)),
        );
    }
    if let Some(filters) = config
        .auto_approve
        .as_ref()
        .and_then(|a| a.filters.as_ref())
    {
        paths.extend(
            filters
                .include_paths
                .iter()
                .flatten()
                .map(|p| ("auto_approve.filters.include_paths".into(), p)),
        );
        paths.extend(
            filters
                .exclude_paths
                .iter()
                .flatten()
                .map(|p| ("auto_approve.filters.exclude_paths".into(), p)),
        );
    }
    if let Some((key, _)) = paths.iter().find(|(_, p)| escapes(p)) {
        return Err(parsed.error(
            Some(key),
            "paths are relative to this folder and can't start with `/` or use `..` or `\\`",
        ));
    }
    // Names the key, never the pattern, which comes from the repository.
    if let Some((key, _)) = paths
        .iter()
        .find(|(key, p)| key.ends_with(".scope") && crate::scope::glob(p).is_err())
    {
        return Err(parsed.error(Some(key), "not a valid glob"));
    }

    let mut items = Vec::new();
    for (key, value) in flatten(&parsed.json) {
        if key != "rules" && key != "context.files" {
            items.push((key.clone(), value, parsed.source(Layer::Repo, &key)));
        }
    }
    let lists = [
        ("rules", parsed.json.get("rules")),
        (
            "context.files",
            parsed.json.get("context").and_then(|c| c.get("files")),
        ),
    ];
    for (key, list) in lists {
        for (i, item) in list
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            items.push((
                key.to_string(),
                item.clone(),
                parsed.source(Layer::Repo, &format!("{key}[{i}]")),
            ));
        }
    }
    Ok(items)
}

pub(crate) fn load_user(text: &str) -> Result<Items, ConfigError> {
    let parsed = parse(USER_FILE, text)?;
    let config: UserConfig = typed(&parsed, text)?;
    check_user_values(&config).map_err(|(key, message)| parsed.error(Some(key), message))?;
    if let Some(models) = &config.models {
        check_models(models).map_err(|(key, message)| parsed.error(Some(&key), message))?;
    }
    for key in ["api_url", "web_url"] {
        if let Some(url) = parsed.json.get(key).and_then(Value::as_str) {
            check_url(key, url, &parsed.source(Layer::User, key))?;
        }
    }
    Ok(flatten(&parsed.json)
        .into_iter()
        .map(|(key, value)| {
            let source = parsed.source(Layer::User, &key);
            (key, value, source)
        })
        .collect())
}

/// `api_url`/`web_url`: an `https://` origin (host and optional port, nothing after
/// but an optional `/`); plain `http://` only for this machine (CONFIGURATION §6).
pub(crate) fn check_url(key: &str, url: &str, source: &Source) -> Result<(), ConfigError> {
    if valid_origin(url) {
        return Ok(());
    }
    Err(ConfigError {
        file: source.file.clone().unwrap_or_else(|| source.to_string()),
        line: source.line,
        key: Some(key.to_string()),
        message: "must be an https:// origin with no path (plain http only for localhost)".into(),
    })
}

fn valid_origin(url: &str) -> bool {
    Origin::parse(url).is_some_and(|o| o.https || o.is_named_loopback())
}

/// A URL origin with nothing after it but an optional `/`: scheme, host and port.
/// Parsed by hand so the rules are exactly these: no userinfo (`a@b`), no path, no
/// query, a DNS name or an IP literal, and a port from 1 to 65535.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// `https://` rather than `http://`.
    pub https: bool,
    /// A DNS name, an IPv4 literal, or an IPv6 literal in brackets (`[::1]`).
    pub host: String,
    /// `None` when the URL has none; the caller picks the default.
    pub port: Option<u16>,
}

impl Origin {
    /// `None` for anything that isn't a plain `http://` or `https://` origin.
    pub fn parse(url: &str) -> Option<Origin> {
        let (authority, https) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
            (Some(rest), _) => (rest, true),
            (_, Some(rest)) => (rest, false),
            _ => return None,
        };
        let authority = authority.strip_suffix('/').unwrap_or(authority);
        // `[v6]:port` or `host:port`, the port optional.
        let (host, port) = match authority.strip_prefix('[') {
            Some(v6) => match v6.split_once(']') {
                Some((inner, tail)) if inner.parse::<std::net::Ipv6Addr>().is_ok() => {
                    match tail.strip_prefix(':') {
                        Some(port) => (format!("[{inner}]"), Some(port)),
                        None if tail.is_empty() => (format!("[{inner}]"), None),
                        None => return None,
                    }
                }
                _ => return None,
            },
            None => match authority.rsplit_once(':') {
                Some((host, port)) => (host.to_string(), Some(port)),
                None => (authority.to_string(), None),
            },
        };
        // A fully qualified name may end in the root's dot (`example.test.`).
        let name = host.strip_suffix('.').unwrap_or(&host);
        let host_ok = host.starts_with('[')
            || (!name.is_empty()
                && !name.starts_with(['.', '-'])
                && !name.ends_with(['.', '-'])
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'));
        let port = match port {
            None => None,
            Some(p) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
                Some(p.parse::<u16>().ok().filter(|&n| n > 0)?)
            }
            Some(_) => return None,
        };
        host_ok.then_some(Origin { https, host, port })
    }

    /// The three spellings of this machine that config accepts over plain HTTP.
    pub fn is_named_loopback(&self) -> bool {
        matches!(self.host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
    }
}
