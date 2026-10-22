// SPDX-License-Identifier: AGPL-3.0-or-later

//! Validation edges: ceilings in nested folders, run flags, value-free errors, URLs.

use std::collections::BTreeMap;

use gitcko_config::{Ceilings, FolderConfig, RepoFiles, RunFlags, resolve_repo, resolve_user};
use gitcko_domain::RepoPath;

fn repo(root: &str, nested: Option<&str>) -> RepoFiles {
    let mut folders = vec![FolderConfig {
        folder: None,
        config_toml: Some(root.into()),
        rules_md: None,
    }];
    if let Some(text) = nested {
        folders.push(FolderConfig {
            folder: Some(RepoPath::parse("pkg").unwrap()),
            config_toml: Some(text.into()),
            rules_md: None,
        });
    }
    RepoFiles {
        folders,
        legacy_root_file: false,
    }
}

fn reviewed() -> RepoPath {
    RepoPath::parse("pkg/a.rs").unwrap()
}

#[test]
fn nested_tier_above_the_ceiling_falls_back_to_the_parent() {
    let files = repo(
        "schema_version = 1\nstrictness = 3\n",
        Some("tier = \"max\"\nstrictness = 1\n"),
    );
    let resolved = resolve_repo(
        &files,
        &reviewed(),
        &RunFlags::default(),
        &Ceilings::local(),
    )
    .unwrap();
    assert_eq!(resolved.repo_config().strictness, Some(3));
    assert_eq!(resolved.repo_config().tier, None);
    assert_eq!(resolved.reason_codes, ["config_invalid"]);
}

fn flags(text: &str) -> Result<RunFlags, gitcko_config::ConfigError> {
    RunFlags::from_table(&toml::from_str(text).unwrap())
}

#[test]
fn run_flags_get_the_same_checks_as_files() {
    for (text, key) in [
        ("\"review.context\" = 200", "review.context"),
        ("\"review.width\" = 0", "review.width"),
        ("instructions = 123", "instructions"),
        ("tier = \"bogus\"", "tier"),
    ] {
        let err = flags(text)
            .err()
            .unwrap_or_else(|| panic!("{text} should be refused"));
        assert!(err.to_string().contains(key), "{text}: {err}");
    }
    let long = format!("instructions = \"{}\"", "x".repeat(4001));
    assert!(flags(&long).is_err());
    assert!(flags("\"review.context\" = 10\ntier = \"base\"").is_ok());
}

#[test]
fn errors_name_the_key_but_never_the_value() {
    for (text, key, value) in [
        (
            "schema_version = 1\nstrictness = \"loud\"\n",
            "strictness",
            "loud",
        ),
        ("schema_version = 1\ntier = \"bogus\"\n", "tier", "bogus"),
        (
            "schema_version = 1\ncomment_types = [\"nitpicks\"]\n",
            "comment_types",
            "nitpicks",
        ),
    ] {
        let err = resolve_repo(
            &repo(text, None),
            &reviewed(),
            &RunFlags::default(),
            &Ceilings::local(),
        )
        .expect_err(text);
        let shown = err.to_string();
        assert!(
            shown.contains(&format!(".gitcko/config.toml:2: {key}:")),
            "{shown}"
        );
        assert!(!shown.contains(value), "{shown}");
    }
}

#[test]
fn urls_must_be_real_origins() {
    let check = |url: &str| {
        resolve_user(
            Some(&format!("api_url = \"{url}\"")),
            &BTreeMap::new(),
            &RunFlags::default(),
        )
    };
    for bad in [
        "https://:443",
        "https://host with space",
        "http://localhost:notaport",
        "https://example.test:99999",
        "https://exa_mple.test",
        "http://example.test",
        "https://example.test/v1",
        "ftp://example.test",
        "https://example.test..",
        "https://[::g]",
        "https://[1:2]",
    ] {
        assert!(check(bad).is_err(), "{bad} should be refused");
    }
    for good in [
        "https://api.example.test",
        "https://api.example.test:8443/",
        "http://localhost:3000",
        "http://127.0.0.1",
        "http://[::1]:8080",
        // A fully qualified name ends in the root's dot; mapped IPv4 has dots in v6.
        "https://api.example.test.",
        "https://[::ffff:192.0.2.1]",
        "https://[2001:db8::1]:8443",
    ] {
        assert!(
            check(good).is_ok(),
            "{good} should be accepted: {:?}",
            check(good).err()
        );
    }
}
