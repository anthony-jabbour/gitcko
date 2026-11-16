// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko setup`: write the two bindings a review needs, and record remote consent
//! (LOCAL_DEPLOYMENT §3, P5_PLAN §5.3). Hardware profiles, model pulls, indexing and
//! MCP registration are not part of this command.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use gitcko_config::{Keychain, RunFlags, resolve_user};
use gitcko_git::Repo;
use gitcko_model::{DestinationClass, resolve};
use gitcko_protocol::config::UserConfig;

use crate::cli::{SetupArgs, SetupProvider};
use crate::config::{env_with_openrouter, gitcko_paths, read_user_file, set_dotted, write_text};
use crate::{Io, read_answer};

/// Run `gitcko setup`.
pub fn run<O, E>(
    args: &SetupArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    keychain: &dyn Keychain,
    stdin_is_tty: bool,
    answers: Option<&Mutex<VecDeque<String>>>,
    io: &Io<O, E>,
) -> u8
where
    O: Write,
    E: Write,
{
    match run_inner(args, cwd, env, keychain, stdin_is_tty, answers, io) {
        Ok(()) => 0,
        Err(err) => {
            for line in &err.lines {
                let _ = writeln!(lock(&io.stderr), "{line}");
            }
            err.code
        }
    }
}

struct Fail {
    code: u8,
    lines: Vec<String>,
}

fn fail(line: &str) -> Fail {
    Fail {
        code: 2,
        lines: vec![line.to_string()],
    }
}

fn run_inner<O, E>(
    args: &SetupArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    keychain: &dyn Keychain,
    stdin_is_tty: bool,
    answers: Option<&Mutex<VecDeque<String>>>,
    io: &Io<O, E>,
) -> Result<(), Fail>
where
    O: Write,
    E: Write,
{
    if Repo::open(cwd).is_err() {
        return Err(fail("not a git repository"));
    }
    let paths = gitcko_paths(env).map_err(|message| fail(&message))?;
    let existing = read_user_file(&paths.config_file).map_err(fail)?;
    let mut table = match &existing {
        Some(text) => load_existing(text, env)?,
        None => toml::Table::new(),
    };
    if let Some(name) = &args.accept_remote
        && name != "openrouter"
    {
        return Err(fail("unknown provider; expected openrouter"));
    }
    let cred_env = env_with_openrouter(env, keychain).map_err(|message| fail(&message))?;
    let has_key = cred_env
        .get("OPENROUTER_API_KEY")
        .is_some_and(|value| !value.is_empty());
    let headless = is_headless(args.no_input, stdin_is_tty, env);
    let (provider, model) = choose(args, has_key, headless, stdin_is_tty, answers, io)?;
    if provider == SetupProvider::Ollama && args.accept_remote.is_some() {
        return Err(fail("ollama is local and is not a remote acknowledgement"));
    }
    write_bindings(&mut table, provider, &model)?;
    let draft = toml_text(&table)?;
    let user = validate_user(&draft, env)?;
    if provider == SetupProvider::Ollama {
        ensure_local(&user, &cred_env)?;
    }

    let mut disclosed = false;
    if provider == SetupProvider::Openrouter && args.accept_remote.is_none() {
        disclose_remote(&model, io);
        disclosed = true;
        let answer = ask(
            "Acknowledge sending selected code to openrouter? [y/N]:",
            stdin_is_tty,
            answers,
            io,
        )?;
        if !is_yes(&answer) {
            return Err(fail("remote setup was not acknowledged"));
        }
    }
    if provider == SetupProvider::Openrouter && !has_key {
        let typed = ask("OpenRouter API key:", stdin_is_tty, answers, io)?;
        let typed = typed.trim();
        if typed.is_empty() {
            return Err(fail("missing OPENROUTER_API_KEY"));
        }
        store_key(&paths.credentials_file, keychain, typed)?;
    }
    if provider == SetupProvider::Openrouter {
        set_ack(&mut table)?;
        let draft = toml_text(&table)?;
        let user = validate_user(&draft, env)?;
        let cred_env = env_with_openrouter(env, keychain).map_err(|message| fail(&message))?;
        ensure_resolves(&user, &cred_env)?;
        if !disclosed {
            disclose_remote(&model, io);
        }
    } else {
        let _ = writeln!(lock(&io.stderr), "local: code stays on this machine");
    }
    let final_text = toml_text(&table)?;
    write_text(&paths.config_file, &final_text, 0o644).map_err(fail)?;
    Ok(())
}

fn is_headless(no_input: bool, stdin_is_tty: bool, env: &BTreeMap<String, String>) -> bool {
    no_input || !stdin_is_tty || matches!(env.get("CI").map(String::as_str), Some("true" | "1"))
}

fn choose<O, E>(
    args: &SetupArgs,
    has_key: bool,
    headless: bool,
    stdin_is_tty: bool,
    answers: Option<&Mutex<VecDeque<String>>>,
    io: &Io<O, E>,
) -> Result<(SetupProvider, String), Fail>
where
    O: Write,
    E: Write,
{
    let mut missing = Vec::new();
    let provider = match args.provider {
        Some(provider) => Some(provider),
        None if headless => {
            missing.push("missing --provider".to_string());
            None
        }
        None => Some(parse_provider(&ask(
            "provider [ollama|openrouter]:",
            stdin_is_tty,
            answers,
            io,
        )?)?),
    };
    let model = match args
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        Some(model) => Some(model.to_string()),
        None if headless => {
            missing.push("missing --model".to_string());
            None
        }
        None => {
            let typed = ask("model:", stdin_is_tty, answers, io)?;
            let typed = typed.trim();
            if typed.is_empty() {
                return Err(fail("missing --model"));
            }
            Some(typed.to_string())
        }
    };
    if provider == Some(SetupProvider::Openrouter) && args.accept_remote.is_none() && headless {
        missing.push("missing --accept-remote openrouter".to_string());
    }
    if provider == Some(SetupProvider::Openrouter) && !has_key && headless {
        missing.push("missing OPENROUTER_API_KEY".to_string());
    }
    if !missing.is_empty() {
        return Err(Fail {
            code: 2,
            lines: missing,
        });
    }
    let provider = provider.ok_or_else(|| fail("missing --provider"))?;
    let model = model.ok_or_else(|| fail("missing --model"))?;
    Ok((provider, model))
}

fn ask<O, E>(
    prompt: &str,
    stdin_is_tty: bool,
    answers: Option<&Mutex<VecDeque<String>>>,
    io: &Io<O, E>,
) -> Result<String, Fail>
where
    O: Write,
    E: Write,
{
    let _ = writeln!(lock(&io.stderr), "{prompt}");
    let _ = lock(&io.stderr).flush();
    read_answer(answers, stdin_is_tty).ok_or_else(|| fail(&format!("missing {prompt}")))
}

fn parse_provider(text: &str) -> Result<SetupProvider, Fail> {
    match text.trim().to_ascii_lowercase().as_str() {
        "ollama" => Ok(SetupProvider::Ollama),
        "openrouter" => Ok(SetupProvider::Openrouter),
        _ => Err(fail("expected ollama or openrouter")),
    }
}

fn is_yes(text: &str) -> bool {
    matches!(text.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn write_bindings(
    table: &mut toml::Table,
    provider: SetupProvider,
    model: &str,
) -> Result<(), Fail> {
    let name = match provider {
        SetupProvider::Ollama => "ollama",
        SetupProvider::Openrouter => "openrouter",
    };
    for binding in ["review", "risk-classifier"] {
        put(
            table,
            &format!("models.bindings.{binding}.provider"),
            toml::Value::String(name.to_string()),
        )?;
        put(
            table,
            &format!("models.bindings.{binding}.model"),
            toml::Value::String(model.to_string()),
        )?;
    }
    put(
        table,
        "models.roles.review",
        toml::Value::String("review".to_string()),
    )?;
    put(
        table,
        "models.roles.judge",
        toml::Value::String("review".to_string()),
    )?;
    put(
        table,
        "models.roles.risk-classifier",
        toml::Value::String("risk-classifier".to_string()),
    )?;
    Ok(())
}

fn set_ack(table: &mut toml::Table) -> Result<(), Fail> {
    put(
        table,
        "privacy.remote_acknowledged",
        toml::Value::Array(vec![toml::Value::String("openrouter".to_string())]),
    )
}

fn put(table: &mut toml::Table, key: &str, value: toml::Value) -> Result<(), Fail> {
    set_dotted(table, key, value).map_err(|()| fail("could not write the file"))
}

fn load_existing(text: &str, env: &BTreeMap<String, String>) -> Result<toml::Table, Fail> {
    validate_user(text, env)?;
    toml::from_str(text).map_err(|_| fail("could not read the user config"))
}

fn validate_user(text: &str, env: &BTreeMap<String, String>) -> Result<UserConfig, Fail> {
    let resolved = resolve_user(Some(text), env, &RunFlags::default())
        .map_err(|err| fail(&err.to_string()))?;
    Ok(resolved.user_config().clone())
}

fn ensure_local(user: &UserConfig, env: &BTreeMap<String, String>) -> Result<(), Fail> {
    let models = user.models.clone().unwrap_or_default();
    match resolve(&models, env) {
        Ok(resolved)
            if resolved
                .bindings
                .values()
                .any(|binding| binding.destination.class == DestinationClass::CustomerRemote) =>
        {
            Err(fail(
                "OLLAMA_HOST is not a loopback address, so this would not be a local setup",
            ))
        }
        Ok(_) => Ok(()),
        Err(err) => Err(fail(&err.to_string())),
    }
}

fn ensure_resolves(user: &UserConfig, env: &BTreeMap<String, String>) -> Result<(), Fail> {
    let models = user.models.clone().unwrap_or_default();
    resolve(&models, env)
        .map(|_| ())
        .map_err(|err| fail(&err.to_string()))
}

fn store_key(path: &Path, keychain: &dyn Keychain, key: &str) -> Result<(), Fail> {
    if keychain.set("openrouter", key).is_ok() {
        return Ok(());
    }
    let mut openrouter = toml::Table::new();
    openrouter.insert("api_key".into(), toml::Value::String(key.to_string()));
    let mut providers = toml::Table::new();
    providers.insert("openrouter".into(), toml::Value::Table(openrouter));
    let mut root = toml::Table::new();
    root.insert("providers".into(), toml::Value::Table(providers));
    let text = toml::to_string(&root).map_err(|_| fail("could not write the file"))?;
    write_text(path, &text, 0o600).map_err(fail)
}

fn toml_text(table: &toml::Table) -> Result<String, Fail> {
    toml::to_string(table).map_err(|_| fail("could not write the file"))
}

fn disclose_remote<O, E>(model: &str, io: &Io<O, E>)
where
    O: Write,
    E: Write,
{
    let _ = writeln!(
        lock(&io.stderr),
        "remote: selected code is sent to openrouter ({model})"
    );
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}
