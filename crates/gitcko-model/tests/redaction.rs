// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keys and untrusted values never reach `Display` or `Debug` (MODEL_ROUTING §11).

use std::collections::BTreeMap;
use std::time::Duration;

use gitcko_model::{ErrorKind, Limit, ProviderError, ResolveError, Secret, TimeoutKind, resolve};
use gitcko_protocol::config::{ModelRole, UserConfig};

#[test]
fn canary_a_secret_never_prints() {
    for canary in gitcko_redact::canaries() {
        let secret = Secret::new(canary.clone());
        for shown in [
            format!("{secret}"),
            format!("{secret:?}"),
            format!("{:?}", Some(&secret)),
        ] {
            assert_eq!(shown.replace("Some(", "").replace(')', ""), "<secret>");
            assert!(!shown.contains(canary.as_str()));
        }
        assert_eq!(secret.expose(), canary);
    }
}

#[test]
fn canary_resolved_bindings_never_print_their_key() {
    let config: UserConfig = toml::from_str(
        "[models.bindings.smart]\nprovider = \"openrouter\"\nmodel = \"anthropic/example-1\"",
    )
    .unwrap();
    for canary in gitcko_redact::canaries() {
        let env = BTreeMap::from([("OPENROUTER_API_KEY".to_string(), canary.clone())]);
        let resolved = resolve(config.models.as_ref().unwrap(), &env).unwrap();
        let shown = format!("{resolved:?} {resolved:#?}");
        assert!(!shown.contains(canary.as_str()), "{shown}");
        assert!(shown.contains("<secret>"));
    }
}

#[test]
fn canary_a_bad_ollama_host_is_named_but_never_shown() {
    let config: UserConfig =
        toml::from_str("[models.bindings.fast]\nprovider = \"ollama\"\nmodel = \"m:1\"").unwrap();
    for canary in gitcko_redact::canaries() {
        let env = BTreeMap::from([("OLLAMA_HOST".to_string(), canary.clone())]);
        let err = resolve(config.models.as_ref().unwrap(), &env).unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(shown.contains("OLLAMA_HOST"), "{shown}");
        assert!(!shown.contains(canary.as_str()), "{shown}");
    }
}

#[test]
fn every_error_kind_displays_without_values() {
    // No variant can hold a provider body, header or prompt: its fields are numbers,
    // durations and fixed strings. This pins the messages so a new field shows up here.
    let kinds = [
        ErrorKind::Auth,
        ErrorKind::InsufficientCredits,
        ErrorKind::RateLimited {
            retry_after: Some(Duration::from_secs(7)),
        },
        ErrorKind::Overloaded,
        ErrorKind::Server,
        ErrorKind::BadRequest,
        ErrorKind::ContextLength,
        ErrorKind::ContentPolicy,
        ErrorKind::Timeout(TimeoutKind::FirstToken),
        ErrorKind::Connect,
        ErrorKind::Canceled,
        ErrorKind::InvalidResponse("truncated stream"),
        ErrorKind::TooLarge("line"),
        ErrorKind::LocalityViolation,
        ErrorKind::BudgetExhausted(Limit::OutputTokens),
    ];
    let shown: Vec<String> = kinds
        .into_iter()
        .map(|kind| {
            ProviderError {
                kind,
                status: Some(429),
            }
            .to_string()
        })
        .collect();
    insta::assert_debug_snapshot!(shown);
    let missing = ResolveError::MissingCredential {
        var: "OPENROUTER_API_KEY",
        provider: "openrouter",
    };
    assert_eq!(
        missing.to_string(),
        "OPENROUTER_API_KEY is not set; an openrouter binding needs it"
    );
    assert!(
        ResolveError::MissingBinding(ModelRole::Judge)
            .to_string()
            .contains("role judge")
    );
}
