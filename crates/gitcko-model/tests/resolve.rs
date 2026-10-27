// SPDX-License-Identifier: AGPL-3.0-or-later

//! `[models]` + environment → bindings, destinations and disclosure.

use std::collections::BTreeMap;

use gitcko_model::{
    DestinationClass, ResolveError, ResolvedModels, class_of, disclose, is_loopback_host, resolve,
};
use gitcko_protocol::config::{
    DataPolicy, ModelClass, ModelFamily, ModelRole, ModelsConfig, UserConfig,
};

fn models(text: &str) -> ModelsConfig {
    toml::from_str::<UserConfig>(text).unwrap().models.unwrap()
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Built at run time so no key-shaped literal is committed (MW-13).
fn fake_key() -> String {
    format!("{}-{}", "test", "not-a-real-key")
}

fn with_key() -> BTreeMap<String, String> {
    env(&[("OPENROUTER_API_KEY", &fake_key())])
}

const MIXED: &str = r#"
[models.bindings.smart]
provider = "openrouter"
model = "anthropic/example-1"

[models.bindings.fast]
provider = "ollama"
model = "example-coder:7b"

[models.bindings.alt]
provider = "openrouter"
model = "openai/example-2"
data_policy = "zdr"
family = "google"

[models.roles]
judge = "alt"

[models.fallbacks]
fast = ["fast", "smart"]
"#;

fn mixed() -> ResolvedModels {
    resolve(&models(MIXED), &with_key()).unwrap()
}

#[test]
fn roles_use_their_class_binding_unless_overridden() {
    let resolved = mixed();
    assert_eq!(
        resolved.binding_for(ModelRole::Review).unwrap().name,
        "smart"
    );
    assert_eq!(
        resolved.binding_for(ModelRole::Summary).unwrap().name,
        "fast"
    );
    assert_eq!(resolved.binding_for(ModelRole::Judge).unwrap().name, "alt");
    // No `deep` or `embed` binding: those roles are not configured.
    assert!(resolved.binding_for(ModelRole::ReviewDeep).is_none());
    assert!(resolved.binding_for(ModelRole::Embeddings).is_none());
    assert_eq!(resolved.fallbacks[&ModelClass::Fast], ["fast", "smart"]);
    assert!(resolved.inversion);
}

#[test]
fn a_required_role_without_a_binding_is_a_startup_error() {
    let resolved = mixed();
    assert_eq!(
        resolved.require(&[ModelRole::Review, ModelRole::ReviewDeep]),
        Err(ResolveError::MissingBinding(ModelRole::ReviewDeep))
    );
    let shown = resolved
        .require(&[ModelRole::ReviewDeep])
        .unwrap_err()
        .to_string();
    assert!(shown.contains("review-deep"), "{shown}");
    assert!(
        resolved
            .require(&[ModelRole::Review, ModelRole::Judge])
            .is_ok()
    );
}

#[test]
fn family_comes_from_the_vendor_prefix_unless_set() {
    let resolved = mixed();
    assert_eq!(resolved.bindings["smart"].family, ModelFamily::Anthropic);
    assert_eq!(resolved.bindings["alt"].family, ModelFamily::Google);
    assert_eq!(resolved.bindings["fast"].family, ModelFamily::OpenWeight);
    let other = resolve(
        &models("[models.bindings.smart]\nprovider = \"openrouter\"\nmodel = \"meta/example-3\""),
        &with_key(),
    )
    .unwrap();
    assert_eq!(other.bindings["smart"].family, ModelFamily::OpenWeight);
}

#[test]
fn openrouter_bindings_default_to_deny_collection_and_need_a_key() {
    let resolved = mixed();
    assert_eq!(
        resolved.bindings["smart"].data_policy,
        Some(DataPolicy::DenyCollection)
    );
    assert_eq!(resolved.bindings["alt"].data_policy, Some(DataPolicy::Zdr));
    assert_eq!(resolved.bindings["fast"].data_policy, None);
    assert_eq!(
        resolved.bindings["smart"]
            .credential
            .as_ref()
            .unwrap()
            .expose(),
        fake_key()
    );
    assert!(resolved.bindings["fast"].credential.is_none());
    let err = resolve(&models(MIXED), &BTreeMap::new()).unwrap_err();
    assert_eq!(
        err,
        ResolveError::MissingCredential {
            var: "OPENROUTER_API_KEY",
            provider: "openrouter"
        }
    );
    // An empty variable is as good as unset.
    assert!(resolve(&models(MIXED), &env(&[("OPENROUTER_API_KEY", "")])).is_err());
}

#[test]
fn ollama_endpoint_comes_from_the_binding_then_ollama_host_then_the_default() {
    let fast = |config: &str, vars: &[(&str, &str)]| {
        let mut e = with_key();
        e.extend(env(vars));
        resolve(&models(config), &e).map(|r| r.bindings["fast"].destination.clone())
    };
    let plain = "[models.bindings.fast]\nprovider = \"ollama\"\nmodel = \"m:1\"\n";
    let d = fast(plain, &[]).unwrap();
    assert_eq!(
        (d.host.as_str(), d.port, d.class),
        ("127.0.0.1", 11434, DestinationClass::OnDevice)
    );
    // Ollama's own forms: a bare host, a bare host:port, a full URL.
    let d = fast(plain, &[("OLLAMA_HOST", "localhost")]).unwrap();
    assert_eq!((d.host.as_str(), d.port), ("localhost", 11434));
    let d = fast(plain, &[("OLLAMA_HOST", "127.0.0.1:9000")]).unwrap();
    assert_eq!(d.port, 9000);
    let d = fast(plain, &[("OLLAMA_HOST", "https://ollama.example.test")]).unwrap();
    assert_eq!(
        (d.host.as_str(), d.port, d.class),
        ("ollama.example.test", 443, DestinationClass::CustomerRemote)
    );
    // The binding's endpoint wins over the variable.
    let pinned = format!("{plain}endpoint = \"http://[::1]:8080\"\n");
    let d = fast(&pinned, &[("OLLAMA_HOST", "https://ollama.example.test")]).unwrap();
    assert_eq!(
        (d.host.as_str(), d.port, d.class),
        ("[::1]", 8080, DestinationClass::OnDevice)
    );
    // Plain http to another machine, or anything that isn't an origin, is refused,
    // naming the variable and never echoing it.
    for bad in [
        "0.0.0.0",
        "http://192.0.2.1:11434",
        "http://127.0.0.1@evil.test",
        "ftp://x",
        "http://h/p",
    ] {
        let err = fast(plain, &[("OLLAMA_HOST", bad)]).unwrap_err();
        assert_eq!(
            err,
            ResolveError::BadEndpoint("OLLAMA_HOST".into()),
            "{bad}"
        );
        assert!(!err.to_string().contains(bad), "{bad}: {err}");
    }
}

#[test]
fn ollama_context_defaults_to_the_largest_floor_of_the_classes_using_it() {
    let resolved = mixed();
    // `fast` serves fast roles and the fast fallback list only: the fast floor.
    assert_eq!(resolved.bindings["fast"].context, Some(32_768));
    let config = "[models.bindings.fast]\nprovider = \"ollama\"\nmodel = \"m:1\"\n\
                  [models.roles]\nreview = \"fast\"\n";
    let resolved = resolve(&models(config), &BTreeMap::new()).unwrap();
    assert_eq!(resolved.bindings["fast"].context, Some(131_072));
    let config = "[models.bindings.fast]\nprovider = \"ollama\"\nmodel = \"m:1\"\ncontext = 8192\n";
    let resolved = resolve(&models(config), &BTreeMap::new()).unwrap();
    assert_eq!(resolved.bindings["fast"].context, Some(8192));
    assert_eq!(mixed().bindings["smart"].context, None);
}

#[test]
fn references_to_undefined_bindings_are_refused() {
    // gitcko-config checks this too; resolve takes any ModelsConfig, so it checks again.
    let err = resolve(
        &models("[models.roles]\njudge = \"nowhere\""),
        &BTreeMap::new(),
    )
    .unwrap_err();
    assert_eq!(
        err,
        ResolveError::UnknownBinding {
            key: "models.roles.judge".into()
        }
    );
    let err = resolve(
        &models("[models.fallbacks]\nsmart = [\"nowhere\"]"),
        &BTreeMap::new(),
    )
    .unwrap_err();
    assert_eq!(
        err,
        ResolveError::UnknownBinding {
            key: "models.fallbacks.smart".into()
        }
    );
}

#[test]
fn inversion_can_be_turned_off() {
    let resolved = resolve(
        &models("[models.inversion]\nenabled = false"),
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(!resolved.inversion);
}

#[test]
fn only_loopback_hosts_are_on_device() {
    for host in ["127.0.0.1", "127.8.9.10", "[::1]", "localhost"] {
        assert!(is_loopback_host(host), "{host}");
    }
    for host in [
        "0.0.0.0",
        "192.168.1.10",
        "10.0.0.1",
        "localhost.evil.test",
        "127.0.0.1.nip.io",
        "evil.test",
        "[::]",
        "[::ffff:127.0.0.1]",
        "localhost.",
        "LOCALHOST.evil.test",
        "",
    ] {
        assert!(!is_loopback_host(host), "{host}");
    }
}

#[test]
fn every_role_has_its_documented_class() {
    // MODEL_ROUTING §2/§3: the class table, spot-checked across all four classes.
    assert_eq!(class_of(ModelRole::ReviewLight), ModelClass::Fast);
    assert_eq!(class_of(ModelRole::ReReviewGate), ModelClass::Fast);
    assert_eq!(class_of(ModelRole::Judge), ModelClass::Smart);
    assert_eq!(class_of(ModelRole::MemoryLearner), ModelClass::Smart);
    assert_eq!(class_of(ModelRole::ReviewDeep), ModelClass::Deep);
    assert_eq!(class_of(ModelRole::Embeddings), ModelClass::Embed);
}

#[test]
fn disclosure_lists_every_destination_and_the_most_remote() {
    let disclosure = disclose(&mixed());
    assert_eq!(
        disclosure.most_remote,
        Some(DestinationClass::CustomerRemote)
    );
    insta::assert_json_snapshot!(disclosure);
    let local = resolve(
        &models("[models.bindings.fast]\nprovider = \"ollama\"\nmodel = \"m:1\""),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        disclose(&local).most_remote,
        Some(DestinationClass::OnDevice)
    );
    let none = resolve(&ModelsConfig::default(), &BTreeMap::new()).unwrap();
    assert_eq!(disclose(&none).most_remote, None);
}
