// SPDX-License-Identifier: AGPL-3.0-or-later

//! Env > keychain > 0600 file. Errors never carry the secret.

use std::collections::BTreeMap;

use gitcko_config::credentials::{
    CredError, CredentialsFile, MemoryKeychain, SecretSource, resolve_key,
};
use gitcko_protocol::config::ModelProvider;
use gitcko_redact::canaries;

fn canary_key() -> String {
    canaries()[3].clone()
}

#[test]
fn env_wins_over_keychain_and_file() {
    let key = canary_key();
    let file = CredentialsFile::parse(&format!(
        "[providers.openrouter]\napi_key = \"file-{key}\"\n"
    ))
    .unwrap();
    let mut keychain = MemoryKeychain::default();
    keychain
        .entries
        .insert("openrouter".into(), format!("chain-{key}"));
    let mut env = BTreeMap::new();
    env.insert("OPENROUTER_API_KEY".into(), format!("env-{key}"));
    let got = resolve_key(ModelProvider::Openrouter, &env, &keychain, Some(&file))
        .unwrap()
        .unwrap();
    assert_eq!(got.source(), SecretSource::Env);
    assert_eq!(got.expose(), format!("env-{key}"));
    let shown = format!("{got:?}");
    assert!(!shown.contains(&key), "{shown}");
}

#[test]
fn keychain_wins_over_file_when_env_is_empty() {
    let key = canary_key();
    let file = CredentialsFile::parse(&format!(
        "[providers.openrouter]\napi_key = \"file-{key}\"\n"
    ))
    .unwrap();
    let mut keychain = MemoryKeychain::default();
    keychain
        .entries
        .insert("openrouter".into(), format!("chain-{key}"));
    let got = resolve_key(
        ModelProvider::Openrouter,
        &BTreeMap::new(),
        &keychain,
        Some(&file),
    )
    .unwrap()
    .unwrap();
    assert_eq!(got.source(), SecretSource::Keychain);
    assert_eq!(got.expose(), format!("chain-{key}"));
}

#[test]
fn file_is_used_when_env_and_keychain_are_empty() {
    let key = canary_key();
    let file =
        CredentialsFile::parse(&format!("[providers.openrouter]\napi_key = \"{key}\"\n")).unwrap();
    let got = resolve_key(
        ModelProvider::Openrouter,
        &BTreeMap::new(),
        &MemoryKeychain::default(),
        Some(&file),
    )
    .unwrap()
    .unwrap();
    assert_eq!(got.source(), SecretSource::File);
    assert_eq!(got.expose(), key);
}

#[test]
fn ollama_never_loads_a_key() {
    let key = canary_key();
    let file =
        CredentialsFile::parse(&format!("[providers.openrouter]\napi_key = \"{key}\"\n")).unwrap();
    let mut env = BTreeMap::new();
    env.insert("OPENROUTER_API_KEY".into(), key);
    assert_eq!(
        resolve_key(
            ModelProvider::Ollama,
            &env,
            &MemoryKeychain::default(),
            Some(&file)
        )
        .unwrap(),
        None
    );
}

#[test]
fn unknown_credentials_key_is_named_and_the_value_is_not() {
    let key = canary_key();
    let err = CredentialsFile::parse(&format!("[providers.openrouter]\npassword = \"{key}\"\n"))
        .unwrap_err();
    match &err {
        CredError::UnknownKey { key: name } => {
            assert!(name.contains("password"), "{name}");
        }
        other => panic!("expected UnknownKey, got {other:?}"),
    }
    assert!(!err.to_string().contains(&key), "{err}");
}

#[cfg(unix)]
#[test]
fn a_group_readable_credentials_file_is_refused_and_a_0600_file_is_the_control() {
    use gitcko_config::credentials::load_file;
    use gitcko_test_support::TempDir;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    let key = canary_key();
    let dir = TempDir::new("creds");
    let path = dir.path().join("credentials.toml");
    fs::write(
        &path,
        format!("[providers.openrouter]\napi_key = \"{key}\"\n"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let loaded = load_file(&path).unwrap();
    assert_eq!(loaded.get("openrouter"), Some(key.as_str()));

    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let shown = fs::read_to_string(&path).unwrap();
    assert!(
        shown.contains(&key),
        "control: the file still holds the key"
    );
    let err = load_file(&path).unwrap_err();
    assert_eq!(err, CredError::TooOpen);
    assert!(!err.to_string().contains(&key), "{err}");
}

#[test]
fn layer_zero_does_not_depend_on_the_os_keychain() {
    let manifest = include_str!("../Cargo.toml");
    assert!(
        !manifest.lines().any(|line| line.contains("keyring")),
        "gitcko-config is layer 0; the OS keychain adapter belongs in the CLI"
    );
}

#[test]
fn canary_invalid_toml_never_appears_in_the_error() {
    let key = canary_key();
    let err = CredentialsFile::parse(&format!("not toml {key}")).unwrap_err();
    assert_eq!(err, CredError::Invalid);
    assert!(!err.to_string().contains(&key), "{err}");
}
