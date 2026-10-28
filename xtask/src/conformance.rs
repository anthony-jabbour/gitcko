// SPDX-License-Identifier: AGPL-3.0-or-later

//! `cargo xtask conformance --provider <p> [--live]`: the shared adapter checks
//! (`gitcko_model::conformance`) for one provider. Offline they replay wire fixtures
//! from a local mock server, as CI runs them; `--live` runs only the `live_` tests,
//! against the real provider, and is never run by PR CI.

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

pub fn run(provider: &str, live: bool) -> Result {
    let set = |var: &str| std::env::var_os(var).is_some_and(|v| !v.is_empty());
    crate::run("cargo", &args(provider, live, set)?)
}

#[cfg(test)]
mod tests {
    use super::args;

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
