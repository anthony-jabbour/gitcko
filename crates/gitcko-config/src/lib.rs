// SPDX-License-Identifier: AGPL-3.0-or-later

//! Gitcko's configuration: the cascading `.gitcko/` folders, the user file, env vars
//! and run flags, merged by the rules in CONFIGURATION §2–3 and validated by §11. Every
//! resolved value keeps where it came from (file and line, or env var, flag or
//! default), so `gitcko config` can explain it (F-30).
//!
//! Pure: callers read the files (from a commit or the disk) and pass the text in. This
//! crate never touches the filesystem, which keeps it in layer 0 and easy to test.
//!
//! Not here yet: org and team layers (they need a server, Phase 7+) and the base/head
//! protection for PRs that edit `.gitcko/` (CONFIGURATION §4.2, needs code-host trust,
//! Phase 8). Ceilings are an input, so the server can pass stricter ones later.

mod defaults;
mod file;
mod ignore;
mod pr;
mod scope;

use std::collections::BTreeMap;
use std::fmt;

use gitcko_domain::RepoPath;
use gitcko_protocol::config::{RepoConfig, UserConfig};
use gitcko_protocol::review::Tier;
use serde_json::Value;

pub use file::Origin;
pub use ignore::{IgnoreRules, ignore_rules};
pub use pr::combine_for_pr;

/// Which layer a value came from, lowest precedence first (CONFIGURATION §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Default,
    User,
    Repo,
    Env,
    Run,
}

/// Where one value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub layer: Layer,
    /// A repository-relative file (`packages/api/.gitcko/config.toml`), the user file
    /// as `~/.config/gitcko/config.toml`, or the env var's name. Never a local path.
    pub file: Option<String>,
    pub line: Option<u32>,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.layer, &self.file, self.line) {
            (Layer::Default, ..) => write!(f, "default"),
            (Layer::Run, ..) => write!(f, "run flag"),
            (Layer::Env, Some(var), _) => write!(f, "env {var}"),
            (_, Some(file), Some(line)) => write!(f, "{file}:{line}"),
            (_, Some(file), None) => write!(f, "{file}"),
            (layer, None, _) => write!(f, "{layer:?}"),
        }
    }
}

/// A config problem, naming the file, line and key but never the value, which could be
/// a secret (CONFIGURATION §10–11).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ConfigError {
    pub file: String,
    pub line: Option<u32>,
    pub key: Option<String>,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file)?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        if let Some(key) = &self.key {
            write!(f, ": {key}")?;
        }
        write!(f, ": {}", self.message)
    }
}

/// One folder's `.gitcko/` files, as read by the caller.
#[derive(Debug, Clone)]
pub struct FolderConfig {
    /// `None` for the repository root.
    pub folder: Option<RepoPath>,
    pub config_toml: Option<String>,
    pub rules_md: Option<String>,
}

/// Every `.gitcko/` folder in the repository.
#[derive(Debug, Clone, Default)]
pub struct RepoFiles {
    pub folders: Vec<FolderConfig>,
    /// A `.gitcko.toml` exists at the root: the old single-file format, not read.
    pub legacy_root_file: bool,
}

/// Limits a config may narrow but never widen (CONFIGURATION §2). A value above one is
/// an error, never a silent clamp.
#[derive(Debug, Clone, Copy)]
pub struct Ceilings {
    pub max_tier: Tier,
}

impl Ceilings {
    /// Local and Self-Host runs: Base only (ADR-79).
    pub fn local() -> Ceilings {
        Ceilings {
            max_tier: Tier::Base,
        }
    }
}

/// Settings given for one run (`--tier`, `--json`, an `@gitcko` command). Repository
/// review keys accept only `tier`, `fail_on` and `instructions` this way
/// (CONFIGURATION §2); every user key can be set.
#[derive(Debug, Clone, Default)]
pub struct RunFlags {
    repo: Vec<(String, Value)>,
    user: Vec<(String, Value)>,
}

const REPO_FLAGS: [&str; 3] = ["tier", "fail_on", "instructions"];
const USER_FLAGS: [&str; 8] = [
    "color",
    "telemetry",
    "api_url",
    "web_url",
    "review.output",
    "review.layout",
    "review.context",
    "review.width",
];

impl RunFlags {
    /// From a table of `key = value`, with dotted keys for nested ones
    /// (`"review.output" = "json"`).
    pub fn from_table(table: &toml::Table) -> Result<RunFlags, ConfigError> {
        let mut flags = RunFlags::default();
        let value = serde_json::to_value(table).map_err(|e| flag_error(None, e.to_string()))?;
        for (key, value) in file::flatten(&value) {
            if REPO_FLAGS.contains(&key.as_str()) {
                flags.repo.push((key, value));
            } else if USER_FLAGS.contains(&key.as_str()) {
                flags.user.push((key, value));
            } else {
                return Err(flag_error(
                    Some(key),
                    "can't be set for a single run".into(),
                ));
            }
        }
        // Each flag gets the checks a file value would: its type, then its limits.
        for (key, value) in &flags.repo {
            let config: RepoConfig = serde_json::from_value(nest(key, value))
                .map_err(|e| flag_error(Some(key.clone()), file::value_free(&e.to_string())))?;
            file::check_repo_values(&config)
                .map_err(|(k, m)| flag_error(Some(k.into()), m.into()))?;
        }
        for (key, value) in &flags.user {
            let config: UserConfig = serde_json::from_value(nest(key, value))
                .map_err(|e| flag_error(Some(key.clone()), file::value_free(&e.to_string())))?;
            file::check_user_values(&config)
                .map_err(|(k, m)| flag_error(Some(k.into()), m.into()))?;
        }
        Ok(flags)
    }
}

/// `{"a": {"b": value}}` from `"a.b"`.
fn nest(key: &str, value: &Value) -> Value {
    key.rsplit('.').fold(
        value.clone(),
        |inner, part| serde_json::json!({ part: inner }),
    )
}

fn flag_error(key: Option<String>, message: String) -> ConfigError {
    ConfigError {
        file: "run flags".into(),
        line: None,
        key,
        message,
    }
}

/// A resolved value and every source that contributed to it: one for a replaced value,
/// several for combined ones (instructions, rules, context files).
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub value: Value,
    pub sources: Vec<Source>,
}

/// The merged result, keyed by dotted path (`summary.overview.included`).
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    entries: BTreeMap<String, Entry>,
    /// Free-form `rules.md` text, root first, each with its file.
    pub rules_md: Vec<(String, Source)>,
    /// Problems that didn't stop resolution (a legacy file, a broken nested config).
    pub warnings: Vec<String>,
    /// For the review's `reason_codes`, e.g. `config_invalid`.
    pub reason_codes: Vec<String>,
}

impl Resolved {
    /// One value and its sources, by dotted key. This is what `gitcko config` prints,
    /// so a user can see why a setting has the value it has.
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.get(key)
    }

    /// The resolved repository config as its public type.
    pub fn repo_config(&self) -> RepoConfig {
        serde_json::from_value(self.nested()).expect("validated while resolving")
    }

    /// The resolved user settings as their public type.
    pub fn user_config(&self) -> UserConfig {
        serde_json::from_value(self.nested()).expect("validated while resolving")
    }

    /// `sha256:<hex>` of the resolved values and `rules.md` texts (CONFIGURATION §11),
    /// recorded with the review so two runs can be compared. serde_json keeps object
    /// keys sorted, so the JSON is canonical: formatting and comments don't change it.
    /// Sources are left out, except each `rules.md`'s folder, which decides where it
    /// applies.
    pub fn policy_digest(&self) -> String {
        let rules_md: Vec<(&str, Option<&str>)> = self
            .rules_md
            .iter()
            .map(|(text, source)| (text.as_str(), source.file.as_deref()))
            .collect();
        let canonical = serde_json::json!({ "config": self.nested(), "rules_md": rules_md });
        let hash = gitcko_domain::ContentHash::of(canonical.to_string().as_bytes());
        format!("sha256:{hash}")
    }

    fn nested(&self) -> Value {
        let mut root = serde_json::Map::new();
        for (key, entry) in &self.entries {
            let mut table = &mut root;
            let parts: Vec<&str> = key.split('.').collect();
            for part in &parts[..parts.len() - 1] {
                table = table
                    .entry(part.to_string())
                    .or_insert_with(|| Value::Object(Default::default()))
                    .as_object_mut()
                    .expect("a key is never both a table and a value");
            }
            table.insert(parts[parts.len() - 1].to_string(), entry.value.clone());
        }
        Value::Object(root)
    }

    /// Every value with its sources, one per line, for `gitcko config` and tests.
    pub fn explain(&self) -> String {
        let mut out = String::new();
        for (key, entry) in &self.entries {
            match &entry.value {
                // Combined lists: one line per item, each with its own source.
                Value::Array(items) if key == "rules" || key == "context.files" => {
                    for (i, (item, source)) in items.iter().zip(&entry.sources).enumerate() {
                        out.push_str(&format!("{key}[{i}] = {item}  <- {source}\n"));
                    }
                }
                value => {
                    let sources: Vec<String> =
                        entry.sources.iter().map(|s| s.to_string()).collect();
                    out.push_str(&format!("{key} = {value}  <- {}\n", sources.join(", ")));
                }
            }
        }
        for (text, source) in &self.rules_md {
            out.push_str(&format!("rules.md = {:?}  <- {source}\n", text.trim_end()));
        }
        for warning in &self.warnings {
            out.push_str(&format!("warning: {warning}\n"));
        }
        for code in &self.reason_codes {
            out.push_str(&format!("reason_code: {code}\n"));
        }
        out
    }

    fn set(&mut self, key: String, value: Value, source: Source) {
        self.entries.insert(
            key,
            Entry {
                value,
                sources: vec![source],
            },
        );
    }

    /// Applies one layer's values by the cascade rules (CONFIGURATION §3). Each rule
    /// and context file arrives as its own item, so each keeps its own source.
    fn apply(&mut self, values: Vec<(String, Value, Source)>) {
        // A folder's `disabled_rules` drop rules it inherited, so apply them first.
        // Rules without an id can't be disabled.
        for (key, value, _) in &values {
            if key == "disabled_rules"
                && let Some(rules) = self.entries.remove("rules")
            {
                let ids: Vec<&str> = value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let disabled = |rule: &Value| {
                    rule.get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| ids.contains(&id))
                };
                let items = rules.value.as_array().cloned().unwrap_or_default();
                let (kept, sources): (Vec<Value>, Vec<Source>) = items
                    .into_iter()
                    .zip(rules.sources)
                    .filter(|(rule, _)| !disabled(rule))
                    .unzip();
                if !kept.is_empty() {
                    self.entries.insert(
                        "rules".into(),
                        Entry {
                            value: Value::Array(kept),
                            sources,
                        },
                    );
                }
            }
        }
        for (key, value, source) in values {
            match key.as_str() {
                // Combined: appended after the parents' items.
                "rules" | "context.files" => {
                    let entry = self.entries.entry(key).or_insert_with(|| Entry {
                        value: Value::Array(vec![]),
                        sources: vec![],
                    });
                    entry.value.as_array_mut().expect("a list").push(value);
                    entry.sources.push(source);
                }
                // Concatenated parent → child.
                "instructions" => match self.entries.get_mut("instructions") {
                    Some(entry) => {
                        let joined = format!(
                            "{}\n\n{}",
                            entry.value.as_str().unwrap_or(""),
                            value.as_str().unwrap_or("")
                        );
                        entry.value = Value::String(joined);
                        entry.sources.push(source);
                    }
                    None => self.set(key, value, source),
                },
                // Everything else: the later layer replaces it. Tables merge field by
                // field because each field is its own key.
                _ => self.set(key, value, source),
            }
        }
    }
}

/// The repository config that applies to `reviewed`: built-in defaults, then the root
/// `.gitcko/`, then each folder on the way to the file (closest wins), then run flags.
pub fn resolve_repo(
    files: &RepoFiles,
    reviewed: &RepoPath,
    flags: &RunFlags,
    ceilings: &Ceilings,
) -> Result<Resolved, ConfigError> {
    let mut resolved = Resolved::default();
    resolved.apply(defaults::repo());
    if files.legacy_root_file {
        resolved
            .warnings
            .push(".gitcko.toml is not read; move its settings to .gitcko/config.toml".into());
    }
    let mut on_path: Vec<&FolderConfig> = files
        .folders
        .iter()
        .filter(|f| match &f.folder {
            None => true,
            Some(folder) => reviewed
                .as_str()
                .starts_with(&format!("{}/", folder.as_str())),
        })
        .collect();
    on_path.sort_by_key(|f| {
        f.folder
            .as_ref()
            .map_or(0, |p| p.as_str().split('/').count())
    });
    for folder in on_path {
        let prefix = folder
            .folder
            .as_ref()
            .map_or(String::new(), |p| format!("{}/", p.as_str()));
        if let Some(text) = &folder.config_toml {
            let label = format!("{prefix}.gitcko/config.toml");
            match file::load_repo(&label, text, folder.folder.is_none(), ceilings.max_tier) {
                Ok(values) => resolved.apply(values),
                // A broken nested config fails only reviews under it, which fall back to
                // the parent's config (CONFIGURATION §11). A broken root is fatal.
                Err(err) if folder.folder.is_some() => {
                    resolved
                        .warnings
                        .push(format!("{err}; using the parent folder's config"));
                    if !resolved.reason_codes.iter().any(|c| c == "config_invalid") {
                        resolved.reason_codes.push("config_invalid".into());
                    }
                    continue;
                }
                Err(err) => return Err(err),
            }
        }
        if let Some(text) = &folder.rules_md {
            let source = Source {
                layer: Layer::Repo,
                file: Some(format!("{prefix}.gitcko/rules.md")),
                line: None,
            };
            resolved.rules_md.push((text.clone(), source));
        }
    }
    let run = Source {
        layer: Layer::Run,
        file: None,
        line: None,
    };
    resolved.apply(
        flags
            .repo
            .iter()
            .map(|(k, v)| (k.clone(), v.clone(), run.clone()))
            .collect(),
    );
    check_ceiling(&resolved, ceilings)?;
    serde_json::from_value::<RepoConfig>(resolved.nested())
        .map_err(|e| flag_error(None, e.to_string()))?;
    Ok(resolved)
}

pub(crate) fn tier_rank(tier: Tier) -> u8 {
    match tier {
        Tier::Base => 0,
        Tier::Plus => 1,
        Tier::Max => 2,
        // Auto picks a tier per PR, which needs Cloud's higher tiers.
        Tier::Auto => 3,
    }
}

pub(crate) fn above_ceiling(max: Tier) -> String {
    format!("above this deployment's ceiling ({max:?}); higher tiers need Gitcko Cloud")
}

fn check_ceiling(resolved: &Resolved, ceilings: &Ceilings) -> Result<(), ConfigError> {
    let Some(entry) = resolved.get("tier") else {
        return Ok(());
    };
    let tier: Tier = serde_json::from_value(entry.value.clone())
        .map_err(|e| flag_error(Some("tier".into()), e.to_string()))?;
    if tier_rank(tier) <= tier_rank(ceilings.max_tier) {
        return Ok(());
    }
    let source = entry.sources.last().expect("an entry has a source");
    Err(ConfigError {
        file: source.file.clone().unwrap_or_else(|| source.to_string()),
        line: source.line,
        key: Some("tier".into()),
        message: format!(
            "above this deployment's ceiling ({:?}); higher tiers need Gitcko Cloud",
            ceilings.max_tier
        ),
    })
}

/// The user's settings: defaults, then the user file, then env vars, then run flags;
/// `DO_NOT_TRACK` turns telemetry off whatever the others say (CLI_SPEC §7).
pub fn resolve_user(
    user_file: Option<&str>,
    env: &BTreeMap<String, String>,
    flags: &RunFlags,
) -> Result<Resolved, ConfigError> {
    let mut resolved = Resolved::default();
    resolved.apply(defaults::user());
    if let Some(text) = user_file {
        resolved.apply(file::load_user(text)?);
    }
    let env_source = |var: &str| Source {
        layer: Layer::Env,
        file: Some(var.to_string()),
        line: None,
    };
    let set = |var: &str| env.get(var).filter(|v| !v.is_empty());
    let mut from_env = Vec::new();
    if let Some(url) = set("GITCKO_API_URL") {
        from_env.push((
            "api_url".to_string(),
            Value::String(url.clone()),
            env_source("GITCKO_API_URL"),
        ));
    }
    match set("GITCKO_TELEMETRY").map(String::as_str) {
        Some("1") => from_env.push((
            "telemetry".into(),
            Value::Bool(true),
            env_source("GITCKO_TELEMETRY"),
        )),
        Some("0") => from_env.push((
            "telemetry".into(),
            Value::Bool(false),
            env_source("GITCKO_TELEMETRY"),
        )),
        _ => {}
    }
    if set("NO_COLOR").is_some() {
        from_env.push(("color".into(), Value::Bool(false), env_source("NO_COLOR")));
    } else if set("FORCE_COLOR").is_some() {
        from_env.push(("color".into(), Value::Bool(true), env_source("FORCE_COLOR")));
    }
    resolved.apply(from_env);
    let run = Source {
        layer: Layer::Run,
        file: None,
        line: None,
    };
    resolved.apply(
        flags
            .user
            .iter()
            .map(|(k, v)| (k.clone(), v.clone(), run.clone()))
            .collect(),
    );
    if set("DO_NOT_TRACK").is_some() {
        resolved.set(
            "telemetry".into(),
            Value::Bool(false),
            env_source("DO_NOT_TRACK"),
        );
    }
    for key in ["api_url", "web_url"] {
        if let Some(entry) = resolved.get(key) {
            let source = entry.sources.last().expect("an entry has a source");
            file::check_url(key, entry.value.as_str().unwrap_or(""), source)?;
        }
    }
    serde_json::from_value::<UserConfig>(resolved.nested())
        .map_err(|e| flag_error(None, e.to_string()))?;
    Ok(resolved)
}
