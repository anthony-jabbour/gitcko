// SPDX-License-Identifier: AGPL-3.0-or-later

//! User `config.toml` and `credentials.toml` text: parse or typed error, never panic.
//! A loaded secret's Debug string never contains the secret.

#![no_main]

use std::collections::BTreeMap;

use gitcko_config::credentials::{resolve_key, CredentialsFile, MemoryKeychain};
use gitcko_config::paths::GitckoPaths;
use gitcko_config::{resolve_user, RunFlags};
use gitcko_protocol::config::ModelProvider;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if let Ok(resolved) = resolve_user(Some(text), &BTreeMap::new(), &RunFlags::default()) {
        let _ = resolved.user_config();
        let _ = gitcko_config::has_remote_acknowledgement(
            &resolved.user_config(),
            ModelProvider::Openrouter,
        );
    }
    if let Ok(file) = CredentialsFile::parse(text) {
        if let Ok(Some(secret)) = resolve_key(
            ModelProvider::Openrouter,
            &BTreeMap::new(),
            &MemoryKeychain::default(),
            Some(&file),
        ) {
            let shown = format!("{secret:?}");
            assert_eq!(
                shown,
                format!("<secret> ({:?})", secret.source()),
                "Secret Debug leaked the key"
            );
        }
    }
    let mut env = BTreeMap::new();
    env.insert("HOME".into(), "/home/fuzz".into());
    if text.starts_with('/') {
        env.insert(
            "GITCKO_CONFIG".into(),
            text.lines().next().unwrap_or(text).into(),
        );
    }
    let _ = GitckoPaths::from_env(&env);
});
