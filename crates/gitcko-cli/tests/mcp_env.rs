// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko mcp serve` must merge keychain credentials like `gitcko review`.

use std::collections::BTreeMap;

use gitcko_cli::env_with_openrouter;
use gitcko_config::{CredError, Keychain, MemoryKeychain};

#[test]
fn mcp_serve_path_merges_openrouter_from_the_keychain() {
    let mut env = BTreeMap::from([
        ("HOME".into(), "/home/dev".into()),
        ("APPDATA".into(), "/home/dev".into()),
        ("LOCALAPPDATA".into(), "/home/dev/local".into()),
        ("GITCKO_CONFIG".into(), "/home/dev/config.toml".into()),
        ("GITCKO_CACHE_DIR".into(), "/home/dev/cache".into()),
    ]);
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
    let env = BTreeMap::from([
        ("HOME".into(), "/home/dev".into()),
        ("APPDATA".into(), "/home/dev".into()),
        ("LOCALAPPDATA".into(), "/home/dev/local".into()),
        ("GITCKO_CONFIG".into(), "/home/dev/config.toml".into()),
        ("GITCKO_CACHE_DIR".into(), "/home/dev/cache".into()),
    ]);
    let merged = env_with_openrouter(&env, &BrokenKeychain).expect("merge");
    assert!(merged.get("OPENROUTER_API_KEY").is_none());
}
