// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ignore` patterns matched with gitignore semantics (CONFIGURATION §5).

use gitcko_config::{FolderConfig, RepoFiles, ignore_rules};
use gitcko_domain::RepoPath;

fn root(toml: &str) -> FolderConfig {
    FolderConfig {
        folder: None,
        config_toml: Some(format!("schema_version = 1\n{toml}")),
        rules_md: None,
    }
}

fn nested(folder: &str, toml: &str) -> FolderConfig {
    FolderConfig {
        folder: Some(RepoPath::parse(folder).unwrap()),
        config_toml: Some(toml.to_string()),
        rules_md: None,
    }
}

fn check(files: RepoFiles, cases: &[(&str, bool)]) {
    let rules = ignore_rules(&files).unwrap();
    for (path, expected) in cases {
        let ignored = rules.is_ignored(&RepoPath::parse(path).unwrap());
        assert_eq!(ignored, *expected, "{path}");
    }
}

#[test]
fn defaults_skip_lockfiles_vendor_dist_and_minified_files() {
    check(
        RepoFiles::default(),
        &[
            ("Cargo.lock", true),
            ("web/yarn.lock", true),
            ("vendor/lib/a.rs", true),
            ("web/dist/app.js", true),
            ("web/app.min.js", true),
            ("src/vendor.rs", false),
            ("src/main.rs", false),
            // Matching is case-sensitive, like git's default.
            ("VENDOR/a.rs", false),
        ],
    );
}

#[test]
fn negation_anchoring_and_double_star_follow_gitignore() {
    let files = RepoFiles {
        folders: vec![root(
            r#"ignore = ["*.gen.rs", "!keep.gen.rs", "/build", "docs/**/*.md", "tmp/"]"#,
        )],
        legacy_root_file: false,
    };
    check(
        files,
        &[
            ("src/a.gen.rs", true),
            ("src/keep.gen.rs", false),
            ("build/out.rs", true),
            ("src/build/out.rs", false),
            ("docs/a/b/c.md", true),
            ("docs/c.md", true),
            ("src/c.md", false),
            ("x/tmp/y.rs", true),
            // `tmp/` names a folder, so a file called `tmp` doesn't match.
            ("tmp", false),
            // The root list replaced the defaults.
            ("Cargo.lock", false),
        ],
    );
}

#[test]
fn a_nested_list_replaces_its_parents_and_is_relative_to_its_folder() {
    let files = RepoFiles {
        folders: vec![
            root(r#"ignore = ["*.lock"]"#),
            nested("pkg", r#"ignore = ["generated/", "/top.rs"]"#),
        ],
        legacy_root_file: false,
    };
    check(
        files,
        &[
            ("Cargo.lock", true),
            ("generated/a.rs", false),
            ("pkg/generated/a.rs", true),
            ("pkg/top.rs", true),
            ("pkg/sub/top.rs", false),
            // Arrays replace (CONFIGURATION §3), so the root's `*.lock` doesn't reach here.
            ("pkg/Cargo.lock", false),
            // A sibling folder whose name merely starts with `pkg` isn't inside it.
            ("pkg2/generated/a.rs", false),
        ],
    );
}

#[test]
fn the_deepest_folder_wins_whatever_order_folders_are_listed_in() {
    let files = RepoFiles {
        folders: vec![
            root(r#"ignore = []"#),
            nested("pkg", r#"ignore = ["*.a"]"#),
            nested("pkg/sub", r#"ignore = ["*.b"]"#),
        ],
        legacy_root_file: false,
    };
    check(
        files,
        &[
            ("pkg/x.a", true),
            ("pkg/x.b", false),
            ("pkg/sub/x.a", false),
            ("pkg/sub/x.b", true),
        ],
    );
}

#[test]
fn a_nested_folder_without_its_own_list_inherits_the_parents_from_the_parents_folder() {
    let files = RepoFiles {
        folders: vec![
            root(r#"ignore = ["/only-at-root.rs"]"#),
            nested("pkg", r#"strictness = 3"#),
        ],
        legacy_root_file: false,
    };
    check(
        files,
        &[("only-at-root.rs", true), ("pkg/only-at-root.rs", false)],
    );
}

#[test]
fn canary_an_invalid_pattern_is_an_error_naming_the_key_but_not_the_pattern() {
    for canary in gitcko_redact::canaries() {
        // An unclosed `[` isn't a valid glob.
        let pattern = format!("[{}", canary.replace(['"', '\\', '\n'], ""));
        let files = RepoFiles {
            folders: vec![root(&format!("ignore = [{pattern:?}]"))],
            legacy_root_file: false,
        };
        let Err(err) = ignore_rules(&files) else {
            panic!("{pattern} should be rejected");
        };
        assert_eq!(err.key.as_deref(), Some("ignore"));
        assert_eq!(err.file, ".gitcko/config.toml");
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains(&pattern[1..]), "{shown}");
    }
}
