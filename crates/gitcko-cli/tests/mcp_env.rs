// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko mcp serve` must merge keychain credentials like `gitcko review`.

use gitcko_cli::env_with_openrouter;
use gitcko_config::{CredError, Keychain, MemoryKeychain};
use gitcko_test_support::TempDir;
use std::collections::BTreeMap;

fn home_env(home: &TempDir) -> BTreeMap<String, String> {
    let home_path = home
        .path()
        .canonicalize()
        .unwrap_or_else(|_| home.path().to_path_buf());
    let config = home_path.join("config.toml");
    let cache = home_path.join("cache");
    let _ = std::fs::create_dir_all(&cache);
    BTreeMap::from([
        ("HOME".into(), home_path.display().to_string()),
        ("APPDATA".into(), home_path.display().to_string()),
        (
            "LOCALAPPDATA".into(),
            home_path.join("local").display().to_string(),
        ),
        ("GITCKO_CONFIG".into(), config.display().to_string()),
        ("GITCKO_CACHE_DIR".into(), cache.display().to_string()),
    ])
}

#[test]
fn mcp_serve_path_merges_openrouter_from_the_keychain() {
    let home = TempDir::new("mcp-env-home");
    let mut env = home_env(&home);
    let chain = MemoryKeychain {
        entries: BTreeMap::from([("openrouter".into(), "keychain-secret".into())]),
    };
    let merged = env_with_openrouter(&env, &chain).expect("merge");
    assert_eq!(
        merged.get("OPENROUTER_API_KEY").map(String::as_str),
        Some("keychain-secret")
    );
    env.remove("OPENROUTER_API_KEY");
    let again = env_with_openrouter(&env, &chain).expect("merge");
    assert_eq!(
        again.get("OPENROUTER_API_KEY").map(String::as_str),
        Some("keychain-secret")
    );
}

struct BrokenKeychain;

impl Keychain for BrokenKeychain {
    fn get(&self, _account: &str) -> Result<Option<String>, CredError> {
        Err(CredError::Keychain)
    }

    fn set(&self, _account: &str, _secret: &str) -> Result<(), CredError> {
        Err(CredError::Keychain)
    }
}

#[test]
fn unavailable_keychain_does_not_block_mcp_env_merge() {
    let home = TempDir::new("mcp-env-broken");
    let env = home_env(&home);
    let merged = env_with_openrouter(&env, &BrokenKeychain).expect("merge");
    assert!(!merged.contains_key("OPENROUTER_API_KEY"));
}
