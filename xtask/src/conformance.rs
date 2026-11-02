// SPDX-License-Identifier: AGPL-3.0-or-later

//! `cargo xtask conformance --provider <p> [--live]`: the shared adapter checks
//! (`gitcko_model::conformance`) for one provider. Offline they replay wire fixtures
//! from a local mock server, as CI runs them; `--live` runs only the `live_` tests,
//! against the real provider, and is never run by PR CI.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use crate::Result;

/// The nextest arguments for `provider`, after checking what a live run needs is
/// set. Errors name the variable, never a value.
fn args(provider: &str, live: bool, set: impl Fn(&str) -> bool) -> Result<Vec<&'static str>> {
    let (krate, needs): (&str, &[&str]) = match provider {
        "openrouter" => (
            "gitcko-model-openai",
            &["OPENROUTER_API_KEY", "GITCKO_LIVE_MODEL"],
        ),
        "ollama" => ("gitcko-model-ollama", &["GITCKO_LIVE_MODEL"]),
        _ => {
            return Err(format!(
                "unknown provider {provider:?}; expected openrouter or ollama"
            ));
        }
    };
    let mut args = vec![
        "nextest",
        "run",
        "-p",
        krate,
        "--test",
        "conformance",
        "--no-tests=fail",
    ];
    if live {
        if let Some(missing) = needs.iter().find(|v| !set(v)) {
            return Err(format!("--live needs {missing} set"));
        }
        args.extend(["--run-ignored", "only", "-E", "test(/^live_/)"]);
    }
    Ok(args)
}

/// The arguments and environment for the test run. A live run gets the shell plus `.env`,
/// as `eval` does, so the key needn't be exported; an offline run never reads the file.
fn plan(
    provider: &str,
    live: bool,
    base: BTreeMap<String, String>,
    root: &Path,
) -> Result<(Vec<&'static str>, BTreeMap<String, String>)> {
    let env = if live {
        crate::dotenv::with_file(base, root)?
    } else {
        base
    };
    let set = |var: &str| env.get(var).is_some_and(|v| !v.is_empty());
    Ok((args(provider, live, set)?, env))
}

pub fn run(provider: &str, live: bool) -> Result {
    let (args, env) = plan(
        provider,
        live,
        std::env::vars().collect(),
        &gitcko_test_support::workspace_root(),
    )?;
    let status = Command::new("cargo")
        .args(&args)
        .envs(&env)
        .status()
        .map_err(|e| format!("could not start cargo: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {}` failed", args.join(" ")))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{args, plan};

    #[cfg(unix)]
    #[test]
    fn a_live_run_takes_keys_from_dotenv_and_an_offline_one_never_reads_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = gitcko_test_support::empty_repo("conformance-dotenv");
        let key = format!("{}-{}", "canary", "c4f3-conformance");
        let file = dir.path().join(".env");
        std::fs::write(
            &file,
            format!("OPENROUTER_API_KEY={key}\nOLLAMA_HOST=from-file\n"),
        )
        .unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let base = BTreeMap::from([("GITCKO_LIVE_MODEL".to_string(), "m".to_string())]);
        // GITCKO_LIVE_MODEL is in the shell; the key is only in the file.
        let (_, env) = plan("openrouter", true, base.clone(), dir.path()).unwrap();
        assert_eq!(env["OPENROUTER_API_KEY"], key);
        assert_eq!(env["OLLAMA_HOST"], "from-file");
        // Offline, the file isn't read at all.
        let (_, offline) = plan("openrouter", false, base.clone(), dir.path()).unwrap();
        assert_eq!(offline, base);
    }

    #[test]
    fn an_unknown_provider_fails_clearly() {
        let error = args("nope", false, |_| true).unwrap_err();
        assert!(error.contains("\"nope\"") && error.contains("openrouter or ollama"));
    }

    #[test]
    fn live_without_credentials_names_the_variable() {
        let error = args("openrouter", true, |v| v == "GITCKO_LIVE_MODEL").unwrap_err();
        assert_eq!(error, "--live needs OPENROUTER_API_KEY set");
        let error = args("ollama", true, |_| false).unwrap_err();
        assert_eq!(error, "--live needs GITCKO_LIVE_MODEL set");
    }

    #[test]
    fn offline_runs_the_fixture_tests_and_live_only_the_live_ones() {
        let offline = args("ollama", false, |_| false).unwrap();
        assert!(offline.contains(&"gitcko-model-ollama") && !offline.contains(&"--run-ignored"));
        let live = args("openrouter", true, |_| true).unwrap();
        assert!(live.contains(&"gitcko-model-openai"));
        assert!(live.ends_with(&["--run-ignored", "only", "-E", "test(/^live_/)"]));
    }
}
