// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko doctor`: local checks, with no paid call and no dial outside loopback
//! (P5_PLAN §10.2, LOCAL_DEPLOYMENT §10).

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use gitcko_config::{
    Ceilings, FolderConfig, Keychain, Origin, RepoFiles, RunFlags, resolve_repo, resolve_user,
};
use gitcko_domain::RepoPath;
use gitcko_git::Repo;
use gitcko_model::is_loopback_host;
use gitcko_protocol::config::{ModelBinding, ModelProvider, UserConfig};
use serde::Serialize;

use crate::config::{gitcko_paths, openrouter_secret, read_user_file, secret_label};
use crate::{Io, version_line};

const SCHEMA: &str = "gitcko.doctor/1";
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Run `gitcko doctor`. Exit 0 when no check failed, 1 otherwise. Warnings,
/// including the unsigned dev build, still count as healthy.
pub fn run<O, E>(
    json: bool,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    keychain: &dyn Keychain,
    io: &Io<O, E>,
) -> u8
where
    O: Write,
    E: Write,
{
    let checks = collect(cwd, env, keychain);
    let healthy = checks.iter().all(|check| check.status != "fail");
    if json {
        let report = Report {
            schema_version: SCHEMA,
            healthy,
            checks: &checks,
        };
        match serde_json::to_string(&report) {
            Ok(text) => {
                let _ = writeln!(lock(&io.stdout), "{text}");
            }
            Err(_) => {
                let _ = writeln!(lock(&io.stderr), "could not write the report");
                return 1;
            }
        }
    } else {
        for check in &checks {
            let _ = writeln!(
                lock(&io.stdout),
                "{:<28} {:<4}  {}",
                check.id,
                check.status,
                check.detail
            );
        }
    }
    if healthy { 0 } else { 1 }
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: &'a str,
    healthy: bool,
    checks: &'a [Check],
}

#[derive(Serialize)]
struct Check {
    id: String,
    status: &'static str,
    detail: String,
}

fn collect(cwd: &Path, env: &BTreeMap<String, String>, keychain: &dyn Keychain) -> Vec<Check> {
    let mut checks = vec![
        Check {
            id: "binary".into(),
            status: "pass",
            detail: version_line(),
        },
        Check {
            id: "signature".into(),
            status: "warn",
            detail: "not a signed release build".into(),
        },
        git_check(env),
        repo_check(cwd),
    ];
    let paths = gitcko_paths(env);
    let (user, config) = match &paths {
        Ok(paths) => config_check(cwd, env, paths),
        Err(message) => (None, fail("config", message)),
    };
    checks.push(config);
    if let Some(user) = &user {
        checks.extend(binding_checks(user, env));
    }
    checks.push(credentials_check(
        paths.as_ref().ok(),
        env,
        keychain,
        user.as_ref(),
    ));
    checks.push(match &paths {
        Ok(paths) => cache_check(&paths.cache_dir),
        Err(_) => fail("cache", "could not resolve the cache directory"),
    });
    checks
}

fn git_check(env: &BTreeMap<String, String>) -> Check {
    let mut cmd = match git_in_path(env) {
        Some(path) => Command::new(path),
        None if path_var(env).is_some() => return fail("git", "git is not installed"),
        None => Command::new("git"),
    };
    cmd.arg("--version");
    cmd.env_clear();
    for (key, value) in env {
        cmd.env(key, value);
    }
    match cmd.output() {
        Ok(output) if output.status.success() => Check {
            id: "git".into(),
            status: "pass",
            detail: git_detail(&output.stdout),
        },
        _ => fail("git", "git is not installed"),
    }
}

fn path_var(env: &BTreeMap<String, String>) -> Option<&str> {
    env.get("PATH")
        .or_else(|| env.get("Path"))
        .map(String::as_str)
        .filter(|s| !s.is_empty())
}

fn git_in_path(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let path_var = path_var(env)?;
    for dir in std::env::split_paths(path_var) {
        #[cfg(windows)]
        for name in ["git.exe", "git.cmd", "git.bat", "git"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        #[cfg(not(windows))]
        {
            let candidate = dir.join("git");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn git_detail(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let line = text.lines().next().unwrap_or("").trim();
    let safe = line.starts_with("git version ")
        && line.len() <= 80
        && line.chars().all(|c| c.is_ascii_graphic() || c == ' ');
    if safe {
        line.to_string()
    } else {
        "git is available".into()
    }
}

fn repo_check(cwd: &Path) -> Check {
    if Repo::open(cwd).is_ok() {
        pass("repo", "this directory is a git repository")
    } else {
        fail("repo", "not a git repository")
    }
}

fn config_check(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    paths: &gitcko_config::GitckoPaths,
) -> (Option<UserConfig>, Check) {
    if let Some(message) = repo_config_error(cwd) {
        return (None, fail("config", &message));
    }
    match read_user_file(&paths.config_file) {
        Err(message) => (None, fail("config", message)),
        Ok(None) => (None, warn("config", "no user config yet; run gitcko setup")),
        Ok(Some(text)) => match resolve_user(Some(&text), env, &RunFlags::default()) {
            Ok(resolved) => {
                let user = resolved.user_config().clone();
                let check = if cwd.join(".gitcko.toml").is_file() {
                    warn("config", "a legacy .gitcko.toml is not read")
                } else {
                    pass("config", "user config is valid")
                };
                (Some(user), check)
            }
            Err(err) => (None, fail("config", &err.to_string())),
        },
    }
}

fn repo_config_error(cwd: &Path) -> Option<String> {
    let path = cwd.join(".gitcko").join("config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some("could not read repository config".into()),
    };
    let files = RepoFiles {
        folders: vec![FolderConfig {
            folder: None,
            config_toml: Some(text),
            rules_md: None,
        }],
        legacy_root_file: false,
    };
    let reviewed = RepoPath::parse("README").ok()?;
    resolve_repo(&files, &reviewed, &RunFlags::default(), &Ceilings::local())
        .err()
        .map(|err| err.to_string())
}

fn binding_checks(user: &UserConfig, env: &BTreeMap<String, String>) -> Vec<Check> {
    let Some(models) = &user.models else {
        return Vec::new();
    };
    let Some(bindings) = &models.bindings else {
        return Vec::new();
    };
    bindings
        .iter()
        .map(|(name, binding)| binding_check(name, binding, env))
        .collect()
}

fn binding_check(name: &str, binding: &ModelBinding, env: &BTreeMap<String, String>) -> Check {
    let id = format!("binding.{name}");
    match locate(binding, env) {
        Location::Remote => Check {
            id,
            status: "warn",
            detail: "remote; not probed".into(),
        },
        Location::Unprobed(detail) => Check {
            id,
            status: "warn",
            detail: detail.into(),
        },
        Location::Local { host, port } => {
            if !version_ok(&host, port) {
                return fail(&id, "local; unreachable");
            }
            if !chat_ok(&host, port, &binding.model) {
                return fail(&id, "local; reachable; structured output is not available");
            }
            Check {
                id,
                status: "pass",
                detail: "local; reachable; structured output works".into(),
            }
        }
    }
}

enum Location {
    Local { host: String, port: u16 },
    Remote,
    Unprobed(&'static str),
}

fn locate(binding: &ModelBinding, env: &BTreeMap<String, String>) -> Location {
    if binding.provider != ModelProvider::Ollama {
        return Location::Remote;
    }
    if let Some(endpoint) = &binding.endpoint {
        return locate_url(endpoint, 80);
    }
    let url = match env.get("OLLAMA_HOST").filter(|host| !host.is_empty()) {
        Some(host) if host.contains("://") => host.clone(),
        Some(host) => format!("http://{host}"),
        None => "http://127.0.0.1:11434".into(),
    };
    locate_url(&url, 11434)
}

fn locate_url(url: &str, default_port: u16) -> Location {
    let Some(origin) = Origin::parse(url) else {
        return Location::Remote;
    };
    if !is_loopback_host(&origin.host) {
        return Location::Remote;
    }
    if origin.https {
        return Location::Unprobed("local; https is not probed");
    }
    Location::Local {
        host: origin.host,
        port: origin.port.unwrap_or(default_port),
    }
}

fn version_ok(host: &str, port: u16) -> bool {
    let Some(addr) = loopback_addr(host, port) else {
        return false;
    };
    let request = format!("GET /api/version HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    http(addr, &request).is_some_and(|body| body.contains("version"))
}

fn chat_ok(host: &str, port: u16, model: &str) -> bool {
    let Some(addr) = loopback_addr(host, port) else {
        return false;
    };
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ok"}],
        "stream": false,
        "format": {
            "type": "object",
            "properties": {"ok": {"type": "boolean"}},
            "required": ["ok"]
        }
    })
    .to_string();
    let request = format!(
        "POST /api/chat HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    http(addr, &request).is_some_and(|body| body.contains("\"ok\""))
}

/// Connect only to an address this function built from a loopback literal.
/// `localhost` is pinned to 127.0.0.1 so DNS cannot move the probe.
fn loopback_addr(host: &str, port: u16) -> Option<SocketAddr> {
    if host == "localhost" {
        return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>()
        && ip.is_loopback()
    {
        return Some(SocketAddr::new(IpAddr::V4(ip), port));
    }
    let inner = host.strip_prefix('[')?.strip_suffix(']')?;
    let ip = inner.parse::<Ipv6Addr>().ok()?;
    ip.is_loopback()
        .then_some(SocketAddr::new(IpAddr::V6(ip), port))
}

fn http(addr: SocketAddr, request: &str) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT)).ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 65_536 {
                    break;
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::TimedOut
                    || err.kind() == std::io::ErrorKind::WouldBlock =>
            {
                break;
            }
            Err(_) => return None,
        }
    }
    let text = String::from_utf8(buf).ok()?;
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head.lines().next()?;
    if status.starts_with("HTTP/1.1 200") || status.starts_with("HTTP/1.0 200") {
        Some(body.to_string())
    } else {
        None
    }
}

fn credentials_check(
    paths: Option<&gitcko_config::GitckoPaths>,
    env: &BTreeMap<String, String>,
    keychain: &dyn Keychain,
    user: Option<&UserConfig>,
) -> Check {
    let Some(paths) = paths else {
        return fail("credentials", "could not load credentials");
    };
    let needs_key = user.is_some_and(|user| {
        user.models.as_ref().is_some_and(|models| {
            models.bindings.as_ref().is_some_and(|bindings| {
                bindings
                    .values()
                    .any(|binding| binding.provider == ModelProvider::Openrouter)
            })
        })
    });
    match openrouter_secret(env, paths, keychain) {
        Ok(Some(secret)) => pass("credentials", secret_label(secret.source())),
        Ok(None) if needs_key => fail("credentials", "OPENROUTER_API_KEY is not set"),
        Ok(None) => pass("credentials", "not required"),
        Err(err) => fail("credentials", &err.to_string()),
    }
}

fn cache_check(path: &Path) -> Check {
    let meta = match fs::metadata(path) {
        Ok(meta) if meta.is_dir() => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return pass("cache", "absent");
        }
        _ => return fail("cache", "could not read the cache"),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return fail("cache", "readable by the group or others");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
    }
    pass("cache", "private")
}

fn pass(id: &str, detail: &str) -> Check {
    Check {
        id: id.to_string(),
        status: "pass",
        detail: detail.to_string(),
    }
}

fn warn(id: &str, detail: &str) -> Check {
    Check {
        id: id.to_string(),
        status: "warn",
        detail: detail.to_string(),
    }
}

fn fail(id: &str, detail: &str) -> Check {
    Check {
        id: id.to_string(),
        status: "fail",
        detail: detail.to_string(),
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}
