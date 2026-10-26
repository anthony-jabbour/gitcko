// SPDX-License-Identifier: AGPL-3.0-or-later

//! `[models]` in the user file (MODEL_ROUTING §3): exact model ids, provider-only keys,
//! references that resolve, and errors that name the key but never the value.

use std::collections::BTreeMap;

use gitcko_config::{ConfigError, RunFlags, resolve_user};
use gitcko_protocol::config::{DataPolicy, ModelClass, ModelProvider, ModelRole};

const VALID: &str = r#"
[models.bindings.smart]
provider = "openrouter"
model = "example/model-1"
data_policy = "zdr"

[models.bindings.local_fast]
provider = "ollama"
model = "example-coder:7b"
endpoint = "http://127.0.0.1:11434"
context = 32768

[models.roles]
judge = "smart"
review-light = "local_fast"

[models.fallbacks]
fast = ["local_fast", "smart"]

[models.inversion]
enabled = false
"#;

fn user(text: &str) -> Result<gitcko_config::Resolved, ConfigError> {
    resolve_user(Some(text), &BTreeMap::new(), &RunFlags::default())
}

fn binding(provider: &str, extra: &str) -> String {
    format!("[models.bindings.b]\nprovider = \"{provider}\"\n{extra}\n")
}

#[test]
fn a_full_models_table_resolves_to_typed_config() {
    let models = user(VALID).unwrap().user_config().models.unwrap();
    let bindings = models.bindings.unwrap();
    assert_eq!(bindings["smart"].provider, ModelProvider::Openrouter);
    assert_eq!(bindings["smart"].data_policy, Some(DataPolicy::Zdr));
    assert_eq!(bindings["local_fast"].context, Some(32768));
    assert_eq!(models.roles.unwrap()[&ModelRole::ReviewLight], "local_fast");
    assert_eq!(
        models.fallbacks.unwrap()[&ModelClass::Fast],
        ["local_fast", "smart"]
    );
    assert_eq!(models.inversion.unwrap().enabled, Some(false));
}

#[test]
fn moving_aliases_are_refused_and_exact_ids_accepted() {
    // A review must name the model it ran, so ids that move under you are errors.
    for (provider, model) in [
        ("ollama", "latest"),
        ("ollama", "example-coder"),
        ("ollama", "example-coder:latest"),
        ("ollama", "example-coder:"),
        ("openrouter", "example-model"),
        ("openrouter", "openrouter/auto"),
        ("openrouter", "~example/model-latest"),
        ("openrouter", "example/model:latest"),
        ("openrouter", "example/"),
        ("openrouter", ""),
        ("openrouter", "example/model 1"),
    ] {
        let text = binding(provider, &format!("model = \"{model}\""));
        let err = user(&text)
            .err()
            .unwrap_or_else(|| panic!("{model:?} accepted"));
        assert!(
            err.to_string().contains("models.bindings.b.model:"),
            "{model}: {err}"
        );
    }
    for (provider, model) in [
        ("ollama", "example-coder:7b"),
        ("ollama", "example.test/team/coder:7b-q4"),
        ("openrouter", "example/model-1"),
        ("openrouter", "example/model-1:free"),
    ] {
        let text = binding(provider, &format!("model = \"{model}\""));
        assert!(user(&text).is_ok(), "{model}: {:?}", user(&text).err());
    }
}

#[test]
fn provider_only_keys_are_refused_on_the_other_provider() {
    for (provider, extra, key) in [
        (
            "ollama",
            "model = \"m:1\"\ndata_policy = \"zdr\"",
            "data_policy",
        ),
        (
            "openrouter",
            "model = \"a/b\"\nendpoint = \"http://127.0.0.1:1\"",
            "endpoint",
        ),
        ("openrouter", "model = \"a/b\"\ncontext = 32768", "context"),
    ] {
        let err = user(&binding(provider, extra)).expect_err(extra);
        assert!(
            err.to_string()
                .contains(&format!("models.bindings.b.{key}:")),
            "{err}"
        );
    }
}

#[test]
fn ollama_endpoint_and_context_are_checked() {
    for (extra, key) in [
        ("endpoint = \"http://192.0.2.1:11434\"", "endpoint"),
        ("endpoint = \"https://example.test/api\"", "endpoint"),
        ("context = 1024", "context"),
        ("context = 2000000", "context"),
    ] {
        let text = binding("ollama", &format!("model = \"m:1\"\n{extra}"));
        let err = user(&text).expect_err(extra);
        assert!(
            err.to_string()
                .contains(&format!("models.bindings.b.{key}:")),
            "{err}"
        );
    }
    // A remote Ollama over TLS is allowed; locality is computed, never declared.
    let text = binding(
        "ollama",
        "model = \"m:1\"\nendpoint = \"https://ollama.example.test\"",
    );
    assert!(user(&text).is_ok(), "{:?}", user(&text).err());
}

#[test]
fn a_key_field_in_a_binding_is_an_unknown_key() {
    let text = binding("openrouter", "model = \"a/b\"\napi_key = \"x\"");
    let shown = user(&text).expect_err("api_key").to_string();
    assert!(shown.contains("unknown key"), "{shown}");
}

#[test]
fn binding_names_are_plain() {
    // A dot would split the name when settings are rebuilt from dotted keys.
    for name in ["\"a.b\"", "\"Smart\"", "\"\"", "\"a b\""] {
        let text = format!("[models.bindings.{name}]\nprovider = \"ollama\"\nmodel = \"m:1\"\n");
        let err = user(&text)
            .err()
            .unwrap_or_else(|| panic!("{name} accepted"));
        assert!(err.to_string().contains("models.bindings"), "{err}");
    }
}

#[test]
fn roles_and_fallbacks_must_name_defined_bindings() {
    for (text, key) in [
        ("[models.roles]\njudge = \"nowhere\"", "models.roles.judge"),
        (
            "[models.fallbacks]\nsmart = [\"nowhere\"]",
            "models.fallbacks.smart",
        ),
    ] {
        let shown = user(text).expect_err(text).to_string();
        assert!(shown.contains(&format!("{key}:")), "{shown}");
        assert!(!shown.contains("nowhere"), "{shown}");
    }
}

#[test]
fn models_cant_be_set_for_a_single_run() {
    let table = toml::from_str("\"models.inversion.enabled\" = true").unwrap();
    assert!(RunFlags::from_table(&table).is_err());
}

#[test]
fn canary_secret_shaped_model_values_are_refused_without_showing_them() {
    for canary in gitcko_redact::canaries() {
        let mut table = toml::Table::new();
        let mut b = toml::Table::new();
        b.insert("provider".into(), "openrouter".into());
        b.insert("model".into(), toml::Value::String(canary.clone()));
        let mut bindings = toml::Table::new();
        bindings.insert("b".into(), b.into());
        let mut models = toml::Table::new();
        models.insert("bindings".into(), bindings.into());
        table.insert("models".into(), models.into());
        let shown = user(&toml::to_string(&table).unwrap())
            .expect_err("canary accepted")
            .to_string();
        assert!(
            shown.contains("models.bindings.b.model: looks like a secret"),
            "{shown}"
        );
        assert!(!shown.contains(canary.as_str()), "{shown}");
    }
}
