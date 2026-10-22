// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading one config file: parse with a line for every key, check it (CONFIGURATION
//! §11), and turn it into `(key, value, source)` items for the merge.

use std::collections::BTreeMap;

use gitcko_protocol::config::{RepoConfig, UserConfig};
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
        // Unknown keys are named in the message; otherwise use the key on that line.
        let named = e.message().split('`').nth(1).map(str::to_string);
        let at_line = parsed
            .lines
            .iter()
            .filter(|(_, l)| Some(**l) == line)
            .map(|(k, _)| k.clone())
            .next_back();
        ConfigError {
            file: parsed.label.clone(),
            line,
            key: named.or(at_line).map(|k| strip_indices(&k)),
            message: e.message().to_string(),
        }
    })
}

/// A path or glob in a repo file is relative to its folder and can't leave it.
fn escapes(path: &str) -> bool {
    path.starts_with('/') || path.contains('\\') || path.split('/').any(|part| part == "..")
}

pub(crate) fn load_repo(label: &str, text: &str, is_root: bool) -> Result<Items, ConfigError> {
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
    if config.strictness.is_some_and(|s| !(1..=3).contains(&s)) {
        return Err(parsed.error(Some("strictness"), "must be 1, 2 or 3"));
    }
    if config
        .instructions
        .as_ref()
        .is_some_and(|i| i.chars().count() > 4000)
    {
        return Err(parsed.error(Some("instructions"), "longer than 4,000 characters"));
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
    if let Some(review) = &config.review {
        if review.context.is_some_and(|c| c > 60) {
            return Err(parsed.error(Some("review.context"), "must be 0 to 60"));
        }
        if review.width.is_some_and(|w| !(40..=240).contains(&w)) {
            return Err(parsed.error(Some("review.width"), "must be 40 to 240"));
        }
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

/// `api_url`/`web_url`: an `https://` origin with no path; plain `http://` only for this
/// machine (CONFIGURATION §6).
pub(crate) fn check_url(key: &str, url: &str, source: &Source) -> Result<(), ConfigError> {
    let origin_only = |rest: &str| {
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        !rest.is_empty() && !rest.contains(['/', '?', '#', '@'])
    };
    let loopback = |rest: &str| {
        let host = if rest.starts_with('[') {
            rest.split(']').next().map(|h| format!("{h}]"))
        } else {
            rest.split([':', '/']).next().map(str::to_string)
        };
        matches!(host.as_deref(), Some("localhost" | "127.0.0.1" | "[::1]"))
    };
    let ok = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(rest), _) => origin_only(rest),
        (_, Some(rest)) => origin_only(rest) && loopback(rest),
        _ => false,
    };
    if ok {
        return Ok(());
    }
    Err(ConfigError {
        file: source.file.clone().unwrap_or_else(|| source.to_string()),
        line: source.line,
        key: Some(key.to_string()),
        message: "must be an https:// origin with no path (plain http only for localhost)".into(),
    })
}
