// SPDX-License-Identifier: AGPL-3.0-or-later

//! Any text as a root `.gitcko/config.toml`, a nested one and a user file: loading must
//! return a value or a `ConfigError`, never panic, and a resolved config must convert to
//! its public type.

#![no_main]

use std::collections::BTreeMap;

use gitcko_config::{Ceilings, FolderConfig, RepoFiles, RunFlags, resolve_repo, resolve_user};
use gitcko_domain::RepoPath;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let root = "schema_version = 1\n";
    let files = RepoFiles {
        folders: vec![
            FolderConfig { folder: None, config_toml: Some(text.to_string()), rules_md: None },
            FolderConfig {
                folder: Some(RepoPath::parse("pkg").unwrap()),
                config_toml: Some(text.to_string()),
                rules_md: Some(text.to_string()),
            },
        ],
        legacy_root_file: false,
    };
    let reviewed = RepoPath::parse("pkg/a.rs").unwrap();
    if let Ok(resolved) = resolve_repo(&files, &reviewed, &RunFlags::default(), &Ceilings::local()) {
        let _ = resolved.repo_config();
        let _ = resolved.explain();
    }
    // The same text as a nested config under a valid root.
    let nested = RepoFiles {
        folders: vec![
            FolderConfig { folder: None, config_toml: Some(root.to_string()), rules_md: None },
            files.folders[1].clone(),
        ],
        legacy_root_file: true,
    };
    if let Ok(resolved) = resolve_repo(&nested, &reviewed, &RunFlags::default(), &Ceilings::local()) {
        let _ = resolved.repo_config();
    }
    if let Ok(resolved) = resolve_user(Some(text), &BTreeMap::new(), &RunFlags::default()) {
        let _ = resolved.user_config();
    }
});
