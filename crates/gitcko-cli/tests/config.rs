// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko config`: sources, user get/set/unset, and the refusal to store a secret.

use gitcko_cli::{Io, RunEnv, execute_at};
use gitcko_config::{GitckoPaths, Keychain, MemoryKeychain};
use gitcko_redact::canaries;
use gitcko_test_support::{TempDir, empty_repo};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

struct World {
    repo: TempDir,
    _home: TempDir,
    env: BTreeMap<String, String>,
    config: PathBuf,
}

fn world(user: &str) -> World {
    let repo = empty_repo("config-cli");
    let home = TempDir::new("config-home");
    let home_path = home.path().canonicalize().expect("temp home");
    let config = home_path.join("config.toml");
    if !user.is_empty() {
        std::fs::write(&config, user).unwrap();
    }
    let env = BTreeMap::from([
        ("HOME".into(), home_path.display().to_string()),
        ("APPDATA".into(), home_path.display().to_string()),
        (
            "LOCALAPPDATA".into(),
            home_path.join("local").display().to_string(),
        ),
        ("GITCKO_CONFIG".into(), config.display().to_string()),
        (
            "GITCKO_CACHE_DIR".into(),
            home_path.join("cache").display().to_string(),
        ),
    ]);
    World {
        repo,
        _home: home,
        env,
        config,
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
            keychain,
            answers: None,
        },
        &io,
    );
    (code, io.stdout_text(), io.stderr_text())
}

fn write_repo(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

#[test]
fn help_lists_the_user_subcommands() {
    let world = world("");
    let (code, out, err) = go(&world, &["config", "--help"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("get"), "{out}");
    assert!(out.contains("set"), "{out}");
    assert!(out.contains("unset"), "{out}");
    assert!(out.contains("path"), "{out}");
    assert!(out.contains("--json"), "{out}");
}

#[test]
fn canary_a_relative_config_path_is_refused_without_its_value() {
    let world = world("");
    let (code, out, err) = go(&world, &["config", "path"], None);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, format!("{}\n", world.config.display()));

    let canary = canaries()[1].clone();
    let mut env = world.env.clone();
    env.insert("GITCKO_CONFIG".into(), canary.clone());
    let io = Io::capture();
    let code = execute_at(
        &["config", "path"],
        &RunEnv {
            cwd: world.repo.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            keychain: None,
            answers: None,
        },
        &io,
    );
    let err = io.stderr_text();
    assert_eq!(code, 2, "{}", io.stdout_text());
    assert!(err.contains("GITCKO_CONFIG"), "{err}");
    assert!(!err.contains(&canary), "the rejected path leaked: {err}");
    assert!(!io.stdout_text().contains(&canary));
}

#[test]
fn get_shows_a_default_and_a_file_value_with_its_source() {
    let world = world("color = true\n");
    let (code, out, err) = go(&world, &["config", "get", "telemetry"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("false"), "{out}");
    assert!(out.contains("default"), "{out}");

    let (code, out, err) = go(&world, &["config", "get", "color"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("true"), "{out}");
    assert!(out.contains("~/.config/gitcko/config.toml"), "{out}");

    let (code, _, err) = go(&world, &["config", "get", "review.width"], None);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("review.width is unset"), "{err}");
}

#[test]
fn set_and_unset_round_trip_a_user_value_and_leave_repo_keys_alone() {
    let world = world("color = true\n");
    let (code, _, err) = go(&world, &["config", "set", "telemetry", "true"], None);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = go(&world, &["config", "get", "telemetry"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("true"), "{out}");
    assert!(out.contains("~/.config/gitcko/config.toml"), "{out}");
    let stored = std::fs::read_to_string(&world.config).unwrap();
    assert!(stored.contains("color"), "{stored}");
    assert!(stored.contains("telemetry"), "{stored}");

    let (code, _, err) = go(&world, &["config", "unset", "telemetry"], None);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = go(&world, &["config", "get", "telemetry"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("false"), "{out}");
    assert!(out.contains("default"), "{out}");
    let stored = std::fs::read_to_string(&world.config).unwrap();
    assert!(stored.contains("color"), "unset drops one key: {stored}");
    assert!(!stored.contains("telemetry"), "{stored}");

    let (code, _, err) = go(&world, &["config", "set", "strictness", "1"], None);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("edited by hand"), "{err}");
    let stored = std::fs::read_to_string(&world.config).unwrap();
    assert!(!stored.contains("strictness"), "{stored}");
}

#[test]
fn canary_config_set_refuses_a_secret_and_stores_a_normal_setting() {
    let canary = canaries()[1].clone();
    let world = world("");
    let (code, out, err) = go(
        &world,
        &["config", "set", "models.openrouter.api_key", &canary],
        None,
    );
    assert_eq!(code, 2, "{out}{err}");
    assert!(
        err.contains("keychain") || err.contains("environment"),
        "{err}"
    );
    assert!(
        !out.contains(&canary) && !err.contains(&canary),
        "{out}{err}"
    );
    assert!(
        !world.config.exists()
            || !std::fs::read_to_string(&world.config)
                .unwrap()
                .contains(&canary)
    );

    let (code, out, err) = go(&world, &["config", "set", &canary, "true"], None);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("unknown setting"), "{err}");
    assert!(
        !out.contains(&canary) && !err.contains(&canary),
        "{out}{err}"
    );

    let (code, _, err) = go(&world, &["config", "set", "color", "true"], None);
    assert_eq!(code, 0, "{err}");
    let stored = std::fs::read_to_string(&world.config).unwrap();
    assert!(stored.contains("color"), "{stored}");
    assert!(!stored.contains(&canary), "{stored}");
    let (code, out, err) = go(&world, &["config", "get", "color"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("true"), "{out}");
}

#[test]
fn canary_a_rejected_config_value_is_not_echoed() {
    let canary = canaries()[3].clone();
    let world = world("color = true\n");
    let (code, out, err) = go(&world, &["config", "set", "review.output", &canary], None);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("review.output"), "{err}");
    assert!(
        !out.contains(&canary) && !err.contains(&canary),
        "{out}{err}"
    );
    let stored = std::fs::read_to_string(&world.config).unwrap();
    assert!(!stored.contains(&canary), "{stored}");
    assert!(
        stored.contains("color"),
        "a rejected set leaves the file: {stored}"
    );

    let (code, _out, err) = go(&world, &["config", "set", "review.output", "json"], None);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = go(&world, &["config", "get", "review.output"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("json"), "{out}");
}

#[test]
fn resolved_config_names_the_layer_and_json_keeps_warnings_off_stdout() {
    let world = world("color = true\n");
    write_repo(
        world.repo.path(),
        ".gitcko/config.toml",
        "schema_version = 1\nstrictness = 1\n",
    );
    write_repo(
        world.repo.path(),
        "src/.gitcko/config.toml",
        "strictness = 3\n",
    );
    write_repo(world.repo.path(), ".gitcko.toml", "strictness = 1\n");

    let (code, out, err) = go(&world, &["config"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("strictness = 1"), "{out}");
    assert!(out.contains(".gitcko/config.toml"), "{out}");
    assert!(out.contains("color = true"), "{out}");
    assert!(out.contains("default"), "{out}");
    assert!(err.contains(".gitcko.toml"), "{err}");

    let (code, nested, err) = go(&world, &["config", "src/lib.rs"], None);
    assert_eq!(code, 0, "{err}");
    assert!(nested.contains("strictness = 3"), "{nested}");
    assert!(nested.contains("src/.gitcko/config.toml"), "{nested}");

    let (code, json, err) = go(&world, &["config", "--json"], None);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains(".gitcko.toml"), "{err}");
    assert!(!json.contains(".gitcko.toml is not read"), "{json}");
    let value: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(value["repo"]["strictness"], 1);
    assert_eq!(value["user"]["color"], true);
    let source = value["sources"]["repo"]["strictness"].to_string();
    assert!(source.contains(".gitcko/config.toml"), "{source}");
}

#[test]
fn a_secret_in_every_store_is_shown_as_secret_and_the_env_wins() {
    let env_key = canaries()[1].clone();
    let chain_key = canaries()[3].clone();
    let file_key = canaries()[5].clone();
    let mut world = world("color = true\n");
    world
        .env
        .insert("OPENROUTER_API_KEY".into(), env_key.clone());
    let chain = MemoryKeychain {
        entries: BTreeMap::from([("openrouter".into(), chain_key.clone())]),
    };
    let creds = GitckoPaths::from_env(&world.env)
        .expect("paths")
        .credentials_file;
    std::fs::create_dir_all(creds.parent().unwrap()).unwrap();
    std::fs::write(
        &creds,
        format!("[providers.openrouter]\napi_key = \"{file_key}\"\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let (code, out, err) = go(&world, &["config", "--json"], Some(&chain));
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("<secret> (env)"), "{out}");
    for key in [&env_key, &chain_key, &file_key] {
        assert!(!out.contains(key), "secret leaked into stdout: {out}");
        assert!(!err.contains(key), "secret leaked into stderr: {err}");
    }

    world.env.remove("OPENROUTER_API_KEY");
    let (code, out, err) = go(&world, &["config"], Some(&chain));
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("<secret> (keychain)"), "{out}");
    assert!(
        !out.contains(&chain_key) && !out.contains(&file_key),
        "{out}"
    );

    let empty = MemoryKeychain::default();
    let (code, out, err) = go(&world, &["config", "--json"], Some(&empty));
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("<secret> (file)"), "{out}");
    assert!(!out.contains(&file_key), "{out}");
}

#[test]
fn canary_a_path_outside_the_repository_is_refused_without_echoing_it() {
    let canary = canaries()[1].clone();
    let world = world("");
    let outside = format!("../{canary}");
    let (code, out, err) = go(&world, &["config", &outside], None);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("repository-relative"), "{err}");
    assert!(
        !out.contains(&canary) && !err.contains(&canary),
        "{out}{err}"
    );

    let (code, out, err) = go(&world, &["config", "src/lib.rs"], None);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("strictness"), "{out}");
}
