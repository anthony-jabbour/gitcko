// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko config`: the resolved settings and where each value came from
//! (CLI_SPEC §4, CONFIGURATION §10–11).
//!
//! `get`, `set` and `unset` touch the user file only. A repository file is untrusted,
//! so the CLI never writes one. A secret key is refused before the value is stored
//! or printed: keys live in the keychain, `credentials.toml`, or the environment.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gitcko_config::{
    Ceilings, ConfigError, CredError, Entry, FolderConfig, GitckoPaths, Keychain, RepoFiles,
    RunFlags, Secret, SecretSource, load_credentials_file, resolve_key, resolve_repo, resolve_user,
};
use gitcko_domain::RepoPath;
use gitcko_git::Repo;
use gitcko_protocol::config::ModelProvider;
use serde_json::{Map, Value};

use crate::Io;
use crate::cli::{ConfigArgs, ConfigCmd};

const USER_LABEL: &str = "~/.config/gitcko/config.toml";

const USER_EXACT: &[&str] = &[
    "color",
    "telemetry",
    "api_url",
    "web_url",
    "review.output",
    "review.layout",
    "review.context",
    "review.width",
    "privacy.remote_acknowledged",
    "models.inversion.enabled",
];

const REPO_TOP: &[&str] = &[
    "schema_version",
    "strictness",
    "comment_types",
    "tier",
    "instructions",
    "fail_on",
    "auto_review",
    "review_drafts",
    "skip_auto_review",
    "labels",
    "disabled_labels",
    "include_authors",
    "exclude_authors",
    "include_branches",
    "exclude_branches",
    "include_keywords",
    "ignore_keywords",
    "ignore",
    "check_run",
    "summary_only",
    "fix_links",
    "hide_footer",
    "update_description",
    "auto_approve",
    "rules",
    "disabled_rules",
    "context",
    "summary",
];

const SECRET_PARTS: &[&str] = &[
    "api_key",
    "apikey",
    "token",
    "password",
    "secret",
    "credential",
    "credentials",
    "access_token",
    "refresh_token",
];

struct Fail {
    code: u8,
    message: String,
}

impl Fail {
    fn new(code: u8, message: impl Into<String>) -> Fail {
        Fail {
            code,
            message: message.into(),
        }
    }
}

/// Run `gitcko config`. `keychain` is the store for this invocation; tests pass a
/// fake so the OS keychain is never touched.
pub fn run<O, E>(
    args: &ConfigArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    keychain: &dyn Keychain,
    io: &Io<O, E>,
) -> u8
where
    O: Write,
    E: Write,
{
    match dispatch(args, cwd, env, keychain, io) {
        Ok(code) => code,
        Err(err) => {
            let _ = writeln!(lock(&io.stderr), "{}", err.message);
            err.code
        }
    }
}

fn dispatch<O, E>(
    args: &ConfigArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    keychain: &dyn Keychain,
    io: &Io<O, E>,
) -> Result<u8, Fail>
where
    O: Write,
    E: Write,
{
    match &args.command {
        Some(ConfigCmd::Get { key }) => get(key, env, io),
        Some(ConfigCmd::Set { key, value }) => set(key, value, env),
        Some(ConfigCmd::Unset { key }) => unset(key, env),
        Some(ConfigCmd::Path) => print_path(env, io),
        None => show(args.path.as_deref(), args.json, cwd, env, keychain, io),
    }
}

fn show<O, E>(
    path: Option<&str>,
    json: bool,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    keychain: &dyn Keychain,
    io: &Io<O, E>,
) -> Result<u8, Fail>
where
    O: Write,
    E: Write,
{
    let root = find_root(cwd)?;
    let reviewed = reviewed_path(path)?;
    let files = repo_files(&root)?;
    let repo = resolve_repo(&files, &reviewed, &RunFlags::default(), &Ceilings::local())
        .map_err(config_fail)?;
    let paths = gitcko_paths(env).map_err(|message| Fail::new(2, message))?;
    let user_text = read_user_file(&paths.config_file).map_err(|message| Fail::new(2, message))?;
    let user =
        resolve_user(user_text.as_deref(), env, &RunFlags::default()).map_err(config_fail)?;
    let secret = openrouter_secret(env, &paths, keychain).map_err(cred_fail)?;
    let mut warnings = repo.warnings.clone();
    warnings.extend(user.warnings.iter().cloned());
    for code in repo.reason_codes.iter().chain(user.reason_codes.iter()) {
        warnings.push(format!("reason_code: {code}"));
    }
    for warning in &warnings {
        let _ = writeln!(lock(&io.stderr), "warning: {warning}");
    }
    if json {
        let body = json_document(&repo, &user, secret.as_ref());
        let text = serde_json::to_string(&body)
            .map_err(|_| Fail::new(1, "could not encode the config"))?;
        let _ = writeln!(lock(&io.stdout), "{text}");
    } else {
        let mut out = String::new();
        out.push_str("repo:\n");
        out.push_str(&format_entries(&repo));
        out.push_str("user:\n");
        out.push_str(&format_entries(&user));
        if let Some(secret) = &secret {
            out.push_str(&format!(
                "credentials.openrouter = \"{}\"\n",
                secret_label(secret.source())
            ));
        }
        let _ = write!(lock(&io.stdout), "{out}");
    }
    Ok(0)
}

fn get<O, E>(key: &str, env: &BTreeMap<String, String>, io: &Io<O, E>) -> Result<u8, Fail>
where
    O: Write,
    E: Write,
{
    match classify(key) {
        KeyClass::Secret => return Err(secret_refused()),
        KeyClass::Repo => return Err(repo_key_refused(key)),
        KeyClass::Unknown => return Err(unknown_key()),
        KeyClass::User => {}
    }
    let paths = gitcko_paths(env).map_err(|message| Fail::new(2, message))?;
    let text = read_user_file(&paths.config_file).map_err(|message| Fail::new(2, message))?;
    let resolved = resolve_user(text.as_deref(), env, &RunFlags::default()).map_err(config_fail)?;
    let Some(entry) = resolved.get(key) else {
        return Err(Fail::new(2, format!("{key} is unset")));
    };
    let sources: Vec<String> = entry.sources.iter().map(|s| s.to_string()).collect();
    let _ = writeln!(
        lock(&io.stdout),
        "{}  <- {}",
        entry.value,
        sources.join(", ")
    );
    Ok(0)
}

fn set(key: &str, value: &str, env: &BTreeMap<String, String>) -> Result<u8, Fail> {
    match classify(key) {
        KeyClass::Secret => return Err(secret_refused()),
        KeyClass::Repo => return Err(repo_key_refused(key)),
        KeyClass::Unknown => return Err(unknown_key()),
        KeyClass::User => {}
    }
    let paths = gitcko_paths(env).map_err(|message| Fail::new(2, message))?;
    let existing = read_user_file(&paths.config_file).map_err(|message| Fail::new(2, message))?;
    let mut table = match &existing {
        Some(text) => load_table(text, env)?,
        None => toml::Table::new(),
    };
    let parsed = value_for(key, value);
    set_dotted(&mut table, key, parsed).map_err(|_| unknown_key())?;
    let text =
        toml::to_string_pretty(&table).map_err(|_| Fail::new(2, "could not write the file"))?;
    resolve_user(Some(&text), env, &RunFlags::default()).map_err(config_fail)?;
    write_text(&paths.config_file, &text, 0o644).map_err(|message| Fail::new(2, message))?;
    Ok(0)
}

fn unset(key: &str, env: &BTreeMap<String, String>) -> Result<u8, Fail> {
    match classify(key) {
        KeyClass::Secret => return Err(secret_refused()),
        KeyClass::Repo => return Err(repo_key_refused(key)),
        KeyClass::Unknown => return Err(unknown_key()),
        KeyClass::User => {}
    }
    let paths = gitcko_paths(env).map_err(|message| Fail::new(2, message))?;
    let Some(existing) =
        read_user_file(&paths.config_file).map_err(|message| Fail::new(2, message))?
    else {
        return Ok(0);
    };
    let mut table = load_table(&existing, env)?;
    if !unset_dotted(&mut table, key) {
        return Ok(0);
    }
    let text = if table.is_empty() {
        String::new()
    } else {
        toml::to_string_pretty(&table).map_err(|_| Fail::new(2, "could not write the file"))?
    };
    if !text.is_empty() {
        resolve_user(Some(&text), env, &RunFlags::default()).map_err(config_fail)?;
    }
    write_text(&paths.config_file, &text, 0o644).map_err(|message| Fail::new(2, message))?;
    Ok(0)
}

fn print_path<O, E>(env: &BTreeMap<String, String>, io: &Io<O, E>) -> Result<u8, Fail>
where
    O: Write,
    E: Write,
{
    let paths = gitcko_paths(env).map_err(|message| Fail::new(2, message))?;
    let _ = writeln!(lock(&io.stdout), "{}", paths.config_file.display());
    Ok(0)
}

fn json_document(
    repo: &gitcko_config::Resolved,
    user: &gitcko_config::Resolved,
    secret: Option<&Secret>,
) -> Value {
    let (repo_values, repo_sources) = section(repo);
    let (user_values, user_sources) = section(user);
    let mut sources = Map::new();
    sources.insert("repo".to_string(), repo_sources);
    sources.insert("user".to_string(), user_sources);
    let mut root = Map::new();
    root.insert("repo".to_string(), repo_values);
    root.insert("user".to_string(), user_values);
    root.insert("sources".to_string(), Value::Object(sources));
    if let Some(secret) = secret {
        let mut creds = Map::new();
        creds.insert(
            "openrouter".to_string(),
            Value::String(secret_label(secret.source()).to_string()),
        );
        root.insert("credentials".to_string(), Value::Object(creds));
    }
    Value::Object(root)
}

fn section(resolved: &gitcko_config::Resolved) -> (Value, Value) {
    let mut sources = Map::new();
    for (key, entry) in resolved.entries() {
        let list = entry
            .sources
            .iter()
            .map(|source| Value::String(source.to_string()))
            .collect();
        sources.insert(key.to_string(), Value::Array(list));
    }
    (resolved.values(), Value::Object(sources))
}

fn format_entries(resolved: &gitcko_config::Resolved) -> String {
    let mut out = String::new();
    for (key, entry) in resolved.entries() {
        push_entry(&mut out, key, entry);
    }
    for (text, source) in &resolved.rules_md {
        out.push_str(&format!("rules.md = {:?}  <- {source}\n", text.trim_end()));
    }
    out
}

fn push_entry(out: &mut String, key: &str, entry: &Entry) {
    match &entry.value {
        Value::Array(items) if key == "rules" || key == "context.files" => {
            for (index, (item, source)) in items.iter().zip(&entry.sources).enumerate() {
                out.push_str(&format!("{key}[{index}] = {item}  <- {source}\n"));
            }
        }
        value => {
            let sources: Vec<String> = entry.sources.iter().map(|s| s.to_string()).collect();
            out.push_str(&format!("{key} = {value}  <- {}\n", sources.join(", ")));
        }
    }
}

/// Where one user key sits. Unknown keys are not echoed back: the text may be a secret
/// someone pasted as the key.
enum KeyClass {
    User,
    Repo,
    Secret,
    Unknown,
}

fn classify(key: &str) -> KeyClass {
    if secret_key(key) {
        KeyClass::Secret
    } else if user_key(key) {
        KeyClass::User
    } else if repo_key(key) {
        KeyClass::Repo
    } else {
        KeyClass::Unknown
    }
}

fn secret_key(key: &str) -> bool {
    key.split('.').any(|part| SECRET_PARTS.contains(&part))
}

fn repo_key(key: &str) -> bool {
    key.split('.')
        .next()
        .is_some_and(|top| REPO_TOP.contains(&top))
}

fn user_key(key: &str) -> bool {
    if USER_EXACT.contains(&key) {
        return true;
    }
    let mut parts = key.split('.');
    if parts.next() != Some("models") {
        return false;
    }
    match parts.next() {
        Some("bindings") => {
            let name = parts.next();
            let field = parts.next();
            name.is_some_and(plain_name)
                && matches!(
                    field,
                    Some(
                        "provider"
                            | "model"
                            | "reasoning_effort"
                            | "family"
                            | "data_policy"
                            | "endpoint"
                            | "context"
                    )
                )
                && parts.next().is_none()
        }
        Some("roles") => {
            let name = parts.next();
            name.is_some_and(plain_name) && parts.next().is_none()
        }
        Some("fallbacks") => {
            let class = parts.next();
            matches!(class, Some("fast" | "smart" | "deep" | "embed")) && parts.next().is_none()
        }
        _ => false,
    }
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn secret_refused() -> Fail {
    Fail::new(
        2,
        "can't set a secret here; use the keychain or an environment variable",
    )
}

fn repo_key_refused(key: &str) -> Fail {
    Fail::new(
        2,
        format!("{key} is repository config and is edited by hand"),
    )
}

fn unknown_key() -> Fail {
    Fail::new(2, "unknown setting")
}

fn value_for(key: &str, raw: &str) -> toml::Value {
    if key == "privacy.remote_acknowledged" && !raw.trim_start().starts_with('[') {
        return toml::Value::Array(vec![toml::Value::String(raw.to_string())]);
    }
    if let Some(parsed) = parse_inline(raw)
        && (parsed.is_bool() || parsed.is_integer() || parsed.is_array() || parsed.is_str())
    {
        return parsed;
    }
    toml::Value::String(raw.to_string())
}

/// `v = <raw>` so a bool, number or array is a TOML value. A bare word is not TOML,
/// and the caller keeps it as a string. A failure here is not an error: the word is
/// the value.
fn parse_inline(raw: &str) -> Option<toml::Value> {
    let wrapped = format!("v = {raw}\n");
    let table: toml::Table = toml::from_str(&wrapped).ok()?;
    table.get("v").cloned()
}

fn load_table(text: &str, env: &BTreeMap<String, String>) -> Result<toml::Table, Fail> {
    resolve_user(Some(text), env, &RunFlags::default()).map_err(config_fail)?;
    toml::from_str(text).map_err(|_| Fail::new(2, format!("{USER_LABEL}: could not be read")))
}

pub(crate) fn set_dotted(table: &mut toml::Table, key: &str, value: toml::Value) -> Result<(), ()> {
    let mut parts = key.split('.');
    let Some(head) = parts.next().filter(|part| !part.is_empty()) else {
        return Err(());
    };
    match parts.next() {
        None => {
            table.insert(head.to_string(), value);
            Ok(())
        }
        Some(second) => {
            let rest = std::iter::once(second)
                .chain(parts)
                .collect::<Vec<_>>()
                .join(".");
            if rest.is_empty() || rest.split('.').any(str::is_empty) {
                return Err(());
            }
            if !table.contains_key(head) {
                table.insert(head.to_string(), toml::Value::Table(toml::Table::new()));
            }
            let child = table
                .get_mut(head)
                .and_then(toml::Value::as_table_mut)
                .ok_or(())?;
            set_dotted(child, &rest, value)
        }
    }
}

fn unset_dotted(table: &mut toml::Table, key: &str) -> bool {
    let mut parts = key.split('.');
    let Some(head) = parts.next() else {
        return false;
    };
    match parts.next() {
        None => table.remove(head).is_some(),
        Some(second) => {
            let rest = std::iter::once(second)
                .chain(parts)
                .collect::<Vec<_>>()
                .join(".");
            let Some(child) = table.get_mut(head).and_then(toml::Value::as_table_mut) else {
                return false;
            };
            let removed = unset_dotted(child, &rest);
            if child.is_empty() {
                table.remove(head);
            }
            removed
        }
    }
}

fn reviewed_path(path: Option<&str>) -> Result<RepoPath, Fail> {
    RepoPath::parse(path.unwrap_or("README"))
        .map_err(|_| Fail::new(2, "a path must be repository-relative"))
}

fn find_root(cwd: &Path) -> Result<PathBuf, Fail> {
    Repo::open(cwd).map_err(|_| Fail::new(2, "not a git repository"))?;
    let mut dir = cwd
        .canonicalize()
        .map_err(|_| Fail::new(2, "not a git repository"))?;
    loop {
        if dir.join(".git").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    cwd.canonicalize()
        .map_err(|_| Fail::new(2, "not a git repository"))
}

fn repo_files(root: &Path) -> Result<RepoFiles, Fail> {
    let mut folders = Vec::new();
    walk_repo(root, root, &mut folders)?;
    Ok(RepoFiles {
        folders,
        legacy_root_file: root.join(".gitcko.toml").is_file(),
    })
}

fn walk_repo(root: &Path, dir: &Path, folders: &mut Vec<FolderConfig>) -> Result<(), Fail> {
    let gitcko = dir.join(".gitcko");
    if is_real_dir(&gitcko) {
        let config_toml = read_optional(&gitcko.join("config.toml"))?;
        let rules_md = read_optional(&gitcko.join("rules.md"))?;
        if config_toml.is_some() || rules_md.is_some() {
            folders.push(FolderConfig {
                folder: folder_of(root, dir)?,
                config_toml,
                rules_md,
            });
        }
    }
    let entries = fs::read_dir(dir).map_err(|_| Fail::new(2, "could not read the repository"))?;
    for entry in entries {
        let entry = entry.map_err(|_| Fail::new(2, "could not read the repository"))?;
        if entry.file_name() == ".git" || entry.file_name() == ".gitcko" {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|_| Fail::new(2, "could not read the repository"))?;
        if file_type.is_symlink() || !file_type.is_dir() {
            continue;
        }
        walk_repo(root, &entry.path(), folders)?;
    }
    Ok(())
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_dir())
        .unwrap_or(false)
}

fn folder_of(root: &Path, dir: &Path) -> Result<Option<RepoPath>, Fail> {
    if dir == root {
        return Ok(None);
    }
    let rel = dir
        .strip_prefix(root)
        .map_err(|_| Fail::new(2, "could not read the repository"))?;
    let text = rel
        .to_str()
        .ok_or_else(|| Fail::new(2, "could not read the repository"))?;
    RepoPath::parse(text)
        .map(Some)
        .map_err(|_| Fail::new(2, "could not read the repository"))
}

fn read_optional(path: &Path) -> Result<Option<String>, Fail> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Fail::new(2, "could not read repository config")),
    }
}

/// The user config path and the credential file beside it. The error names the
/// variable (`GITCKO_CONFIG`, `HOME`), never the path that was rejected.
pub(crate) fn gitcko_paths(env: &BTreeMap<String, String>) -> Result<GitckoPaths, String> {
    GitckoPaths::from_env(env).map_err(|err| err.to_string())
}

pub(crate) fn read_user_file(path: &Path) -> Result<Option<String>, &'static str> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("could not read the user config"),
    }
}

/// Write `text` by rename so a crash can't leave a half-written file. `mode` is the
/// unix mode (credentials are `0o600`; the user config is `0o644`). The error is a
/// fixed sentence: the path and the body are not included.
pub(crate) fn write_text(path: &Path, text: &str, mode: u32) -> Result<(), &'static str> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        create_private_dir(parent)?;
    }
    let tmp = tmp_path(path);
    write_tmp(&tmp, text, mode)?;
    fs::rename(&tmp, path).map_err(|_| {
        let _ = fs::remove_file(&tmp);
        "could not write the file"
    })
}

fn create_private_dir(path: &Path) -> Result<(), &'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder
            .recursive(true)
            .create(path)
            .map_err(|_| "could not write the file")
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).map_err(|_| "could not write the file")
    }
}

fn write_tmp(path: &Path, text: &str, mode: u32) -> Result<(), &'static str> {
    let mut file = fs::File::create(path).map_err(|_| "could not write the file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|_| "could not write the file")?;
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }
    file.write_all(text.as_bytes())
        .map_err(|_| "could not write the file")
}

fn tmp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("config.toml");
    path.with_file_name(format!("{name}.tmp"))
}

/// The OpenRouter key, if one is configured. The value stays inside [`Secret`].
pub(crate) fn openrouter_secret(
    env: &BTreeMap<String, String>,
    paths: &GitckoPaths,
    keychain: &dyn Keychain,
) -> Result<Option<Secret>, CredError> {
    let file = match load_credentials_file(&paths.credentials_file) {
        Ok(file) => Some(file),
        Err(CredError::Io { .. }) if !paths.credentials_file.exists() => None,
        Err(err) => return Err(err),
    };
    resolve_key(ModelProvider::Openrouter, env, keychain, file.as_ref())
}

/// A copy of `env` with `OPENROUTER_API_KEY` filled from the keychain or the
/// credentials file when the variable is unset. The returned map is for the model
/// resolver; callers must not print it.
pub(crate) fn env_with_openrouter(
    env: &BTreeMap<String, String>,
    keychain: &dyn Keychain,
) -> Result<BTreeMap<String, String>, String> {
    let mut copy = env.clone();
    if copy
        .get("OPENROUTER_API_KEY")
        .is_some_and(|value| !value.is_empty())
    {
        return Ok(copy);
    }
    let paths = gitcko_paths(env)?;
    match openrouter_secret(env, &paths, keychain) {
        Ok(Some(secret)) => {
            copy.insert("OPENROUTER_API_KEY".into(), secret.expose().to_string());
            Ok(copy)
        }
        Ok(None) => Ok(copy),
        Err(err) => Err(err.to_string()),
    }
}

/// What `config` and `doctor` print for a key. The value is not in here.
pub(crate) fn secret_label(source: SecretSource) -> &'static str {
    match source {
        SecretSource::Env => "<secret> (env)",
        SecretSource::Keychain => "<secret> (keychain)",
        SecretSource::File => "<secret> (file)",
    }
}

fn config_fail(err: ConfigError) -> Fail {
    Fail::new(2, err.to_string())
}

fn cred_fail(err: CredError) -> Fail {
    Fail::new(2, err.to_string())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}
