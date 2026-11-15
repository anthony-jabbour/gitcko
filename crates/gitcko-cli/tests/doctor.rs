// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko doctor`: the `gitcko.doctor/1` report, loopback probes, and no secrets.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gitcko_cli::{Io, RunEnv, execute_at};
use gitcko_config::{CredError, GitckoPaths, Keychain};
use gitcko_redact::canaries;
use gitcko_test_support::{TempDir, empty_repo};

struct World {
    repo: TempDir,
    home: PathBuf,
    _home: TempDir,
    env: BTreeMap<String, String>,
    config: PathBuf,
    cache: PathBuf,
}

fn prepare(repo: TempDir) -> World {
    let home = TempDir::new("doctor-home");
    let home_path = home.path().canonicalize().expect("temp home");
    let config = home_path.join("config.toml");
    let cache = home_path.join("cache");
    let env = BTreeMap::from([
        ("HOME".into(), home_path.display().to_string()),
        ("APPDATA".into(), home_path.display().to_string()),
        (
            "LOCALAPPDATA".into(),
            home_path.join("local").display().to_string(),
        ),
        ("GITCKO_CONFIG".into(), config.display().to_string()),
        ("GITCKO_CACHE_DIR".into(), cache.display().to_string()),
    ]);
    World {
        repo,
        home: home_path,
        _home: home,
        env,
        config,
        cache,
    }
}

struct Store {
    entries: Mutex<BTreeMap<String, String>>,
}

impl Keychain for Store {
    fn get(&self, account: &str) -> Result<Option<String>, CredError> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), CredError> {
        self.entries
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }
}

fn go(world: &World, args: &[&str], keychain: Option<&dyn Keychain>) -> (u8, String, String) {
    let io = Io::capture();
    let code = execute_at(
        args,
        &RunEnv {
            cwd: world.repo.path(),
            env: &world.env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain,
            answers: None,
        },
        &io,
    );
    (code, io.stdout_text(), io.stderr_text())
}

fn check<'a>(report: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == id)
        .unwrap_or_else(|| panic!("missing check {id} in {report}"))
}

#[test]
fn help_lists_json() {
    let world = prepare(empty_repo("doctor-help"));
    let (code, out, err) = go(&world, &["doctor", "--help"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("--json"), "{out}");
}

#[test]
fn json_report_is_healthy_when_nothing_failed() {
    let world = prepare(empty_repo("doctor-ok"));
    let (code, out, err) = go(&world, &["doctor", "--json"], None);
    assert_eq!(code, 0, "{err}{out}");
    assert!(err.is_empty(), "{err}");
    let report: serde_json::Value = serde_json::from_str(out.trim()).expect(&out);
    assert_eq!(report["schema_version"], "gitcko.doctor/1");
    assert_eq!(report["healthy"], true);
    assert_eq!(check(&report, "signature")["status"], "warn");
    assert_eq!(
        check(&report, "signature")["detail"],
        "not a signed release build"
    );
    assert_eq!(check(&report, "config")["status"], "warn");
    assert_eq!(check(&report, "repo")["status"], "pass");
    assert_eq!(check(&report, "cache")["detail"], "absent");
    assert_eq!(check(&report, "credentials")["detail"], "not required");
    assert!(
        check(&report, "binary")["detail"]
            .as_str()
            .unwrap()
            .starts_with("gitcko "),
        "{report}"
    );
    let ids: Vec<&str> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| check["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "binary",
            "signature",
            "git",
            "repo",
            "config",
            "credentials",
            "cache"
        ]
    );
    assert!(out.trim().starts_with('{') && out.trim().ends_with('}'));
}

#[test]
fn text_names_each_check_and_a_missing_repo_fails() {
    let world = prepare(empty_repo("doctor-text"));
    let (code, out, err) = go(&world, &["doctor"], None);
    assert_eq!(code, 0, "{err}{out}");
    assert!(
        out.contains("signature                    warn  not a signed release build"),
        "{out}"
    );
    assert!(
        out.contains("cache                        pass  absent"),
        "{out}"
    );

    let bare = prepare(TempDir::new("doctor-not-a-repo"));
    let (code, out, err) = go(&bare, &["doctor", "--json"], None);
    assert_eq!(code, 1, "{err}{out}");
    let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(report["healthy"], false);
    assert_eq!(check(&report, "repo")["status"], "fail");
    assert_eq!(check(&report, "repo")["detail"], "not a git repository");
}

#[test]
fn git_and_cache_and_config_failures_stay_free_of_paths() {
    let world = prepare(empty_repo("doctor-paths"));
    let canary = canaries()[1].clone();
    let empty = TempDir::new("doctor-no-git");
    let mut env = world.env.clone();
    #[cfg(windows)]
    {
        let shim = empty.path().join("git.cmd");
        std::fs::write(&shim, "@echo off\nexit /b 1\n").unwrap();
        env.insert("PATH".into(), empty.path().display().to_string());
    }
    #[cfg(not(windows))]
    {
        env.insert("PATH".into(), empty.path().display().to_string());
    }
    env.insert(
        "GITCKO_CACHE_DIR".into(),
        world.home.join(&canary).display().to_string(),
    );
    let io = Io::capture();
    let code = execute_at(
        &["doctor", "--json"],
        &RunEnv {
            cwd: world.repo.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
        },
        &io,
    );
    let out = io.stdout_text();
    let err = io.stderr_text();
    assert_eq!(code, 1, "{err}{out}");
    assert!(!out.contains(&canary) && !err.contains(&canary));
    let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(check(&report, "git")["detail"], "git is not installed");

    std::fs::create_dir_all(&world.cache).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&world.cache, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let (code, out, err) = go(&world, &["doctor", "--json"], None);
    assert_eq!(code, 0, "{err}{out}");
    assert_eq!(
        check(
            &serde_json::from_str::<serde_json::Value>(out.trim()).unwrap(),
            "cache"
        )["detail"],
        "private"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&world.cache, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (code, out, err) = go(&world, &["doctor", "--json"], None);
        assert_eq!(code, 1, "{err}{out}");
        let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(
            check(&report, "cache")["detail"],
            "readable by the group or others"
        );
        assert!(!out.contains(&world.cache.display().to_string()));
    }

    std::fs::write(&world.config, format!("color = \"{canary}\"\n")).unwrap();
    let (code, out, err) = go(&world, &["doctor", "--json"], None);
    assert_eq!(code, 1, "{err}");
    assert!(!out.contains(&canary) && !err.contains(&canary), "{out}");
    let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(check(&report, "config")["status"], "fail");

    std::fs::write(&world.config, "color = true\n").unwrap();
    let (code, out, _err) = go(&world, &["doctor", "--json"], None);
    let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    if code == 0 || check(&report, "cache")["status"] == "fail" {
        assert_eq!(check(&report, "config")["status"], "pass");
    }
}

#[test]
fn canary_credentials_are_a_label_in_every_store() {
    let world = prepare(empty_repo("doctor-creds"));
    std::fs::write(
        &world.config,
        "\
[models.bindings.remote]
provider = \"openrouter\"
model = \"anthropic/claude-haiku-5.5\"
",
    )
    .unwrap();
    let env_key = canaries()[1].clone();
    let chain_key = canaries()[3].clone();
    let file_key = canaries()[5].clone();
    let mut env = world.env.clone();
    env.insert("OPENROUTER_API_KEY".into(), env_key.clone());
    let store = Store {
        entries: Mutex::new(BTreeMap::from([("openrouter".into(), chain_key.clone())])),
    };
    let cred = GitckoPaths::from_env(&world.env)
        .expect("paths")
        .credentials_file;
    std::fs::create_dir_all(cred.parent().unwrap()).unwrap();
    std::fs::write(
        &cred,
        format!("[providers.openrouter]\napi_key = \"{file_key}\"\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cred, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let io = Io::capture();
    let code = execute_at(
        &["doctor", "--json"],
        &RunEnv {
            cwd: world.repo.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: Some(&store),
            answers: None,
        },
        &io,
    );
    let out = io.stdout_text();
    let err = io.stderr_text();
    assert_eq!(code, 0, "{err}{out}");
    for secret in [&env_key, &chain_key, &file_key] {
        assert!(!out.contains(secret) && !err.contains(secret), "{out}");
    }
    let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(check(&report, "credentials")["detail"], "<secret> (env)");
    assert_eq!(check(&report, "binding.remote")["status"], "warn");
    assert_eq!(
        check(&report, "binding.remote")["detail"],
        "remote; not probed"
    );

    env.remove("OPENROUTER_API_KEY");
    store.entries.lock().unwrap().clear();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cred, std::fs::Permissions::from_mode(0o644)).unwrap();
        let io = Io::capture();
        let code = execute_at(
            &["doctor", "--json"],
            &RunEnv {
                cwd: world.repo.path(),
                env: &env,
                runner: None,
                stdout_is_tty: false,
                stdin_is_tty: false,
                tui: None,
                keychain: Some(&store),
                answers: None,
            },
            &io,
        );
        let out = io.stdout_text();
        assert_eq!(code, 1, "{}", io.stderr_text());
        assert!(!out.contains(&file_key), "{out}");
        let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(
            check(&report, "credentials")["detail"],
            "credentials file is readable by the group or others"
        );
    }
}

#[test]
fn loopback_probes_pass_and_fail_without_dialing_a_remote_model() {
    let seen = Arc::new(Mutex::new(String::new()));
    let good = serve(true, Arc::clone(&seen));
    let bad = serve(false, Arc::new(Mutex::new(String::new())));
    let world = prepare(empty_repo("doctor-probe"));
    let key = canaries()[1].clone();
    std::fs::write(
        &world.config,
        format!(
            "\
[models.bindings.bad]
provider = \"ollama\"
model = \"fixture:1\"
endpoint = \"http://127.0.0.1:{bad}\"

[models.bindings.closed]
provider = \"ollama\"
model = \"fixture:1\"
endpoint = \"http://127.0.0.1:1\"

[models.bindings.good]
provider = \"ollama\"
model = \"fixture:1\"
endpoint = \"http://127.0.0.1:{good}\"

[models.bindings.remote]
provider = \"openrouter\"
model = \"anthropic/claude-haiku-5.5\"
"
        ),
    )
    .unwrap();
    let mut env = world.env.clone();
    env.insert("OPENROUTER_API_KEY".into(), key.clone());
    let io = Io::capture();
    let code = execute_at(
        &["doctor", "--json"],
        &RunEnv {
            cwd: world.repo.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
        },
        &io,
    );
    let out = io.stdout_text();
    let err = io.stderr_text();
    assert_eq!(code, 1, "{err}{out}");
    assert!(!out.contains(&key) && !err.contains(&key));
    let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(report["healthy"], false);
    assert_eq!(check(&report, "binding.good")["status"], "pass");
    assert_eq!(
        check(&report, "binding.good")["detail"],
        "local; reachable; structured output works"
    );
    assert_eq!(check(&report, "binding.bad")["status"], "fail");
    assert!(
        check(&report, "binding.bad")["detail"]
            .as_str()
            .unwrap()
            .contains("structured output"),
        "{report}"
    );
    assert_eq!(check(&report, "binding.closed")["status"], "fail");
    assert_eq!(
        check(&report, "binding.closed")["detail"],
        "local; unreachable"
    );
    assert_eq!(
        check(&report, "binding.remote")["detail"],
        "remote; not probed"
    );
    let ids: Vec<&str> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|check| check["id"].as_str())
        .filter(|id| id.starts_with("binding."))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted);
    let traffic = seen.lock().unwrap().clone();
    assert!(traffic.contains("GET /api/version"), "{traffic}");
    assert!(traffic.contains("POST /api/chat"), "{traffic}");
    assert!(traffic.contains("fixture:1"), "{traffic}");
    assert!(!traffic.contains("claude"), "{traffic}");
    assert!(!traffic.contains(&key), "{traffic}");
}

#[test]
fn a_remote_ollama_host_is_not_probed_and_not_printed() {
    let world = prepare(empty_repo("doctor-remote-ollama"));
    std::fs::write(
        &world.config,
        "\
[models.bindings.localish]
provider = \"ollama\"
model = \"fixture:1\"
",
    )
    .unwrap();
    let mut env = world.env.clone();
    env.insert(
        "OLLAMA_HOST".into(),
        "https://ollama.example.test:11434".into(),
    );
    let io = Io::capture();
    let code = execute_at(
        &["doctor", "--json"],
        &RunEnv {
            cwd: world.repo.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
        },
        &io,
    );
    let out = io.stdout_text();
    assert!(!out.contains("example.test"), "{out}");
    assert!(!io.stderr_text().contains("example.test"));
    let report: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(
        check(&report, "binding.localish")["detail"],
        "remote; not probed"
    );
    let _ = code;
}

fn serve(schema: bool, seen: Arc<Mutex<String>>) -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for _ in 0..4 {
            let Ok((mut sock, _)) = listener.accept() else {
                break;
            };
            let req = read_req(&mut sock);
            {
                let mut all = seen.lock().unwrap_or_else(|err| err.into_inner());
                all.push_str(&req);
            }
            let ok = req.starts_with("GET /api/version")
                || (schema && req.starts_with("POST /api/chat"));
            let body = if req.starts_with("GET") {
                r#"{"version":"0"}"#
            } else {
                r#"{"ok":true}"#
            };
            let status = if ok { "200 OK" } else { "500 ERR" };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes());
        }
    });
    port
}

fn read_req(sock: &mut TcpStream) -> String {
    sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|window| window == b"\r\n\r\n") && buf.len() < 8192 {
        match sock.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}
