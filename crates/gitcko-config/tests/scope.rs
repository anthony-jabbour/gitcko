// SPDX-License-Identifier: AGPL-3.0-or-later

//! `scope` globs on `[[rules]]` and `[[context.files]]`, relative to the folder whose
//! `.gitcko/` defines them (CONFIGURATION §5, REVIEW_RULES §10).

use gitcko_config::{Ceilings, FolderConfig, RepoFiles, RunFlags, resolve_repo};
use gitcko_domain::RepoPath;

fn folder(path: Option<&str>, toml: &str) -> FolderConfig {
    FolderConfig {
        folder: path.map(|p| RepoPath::parse(p).unwrap()),
        config_toml: Some(match path {
            None => format!("schema_version = 1\n{toml}"),
            Some(_) => toml.to_string(),
        }),
        rules_md: None,
    }
}

fn files(folders: Vec<FolderConfig>) -> RepoFiles {
    RepoFiles {
        folders,
        legacy_root_file: false,
    }
}

/// The ids of the rules that apply when reviewing `path`.
fn rule_ids(files: &RepoFiles, path: &str) -> Vec<String> {
    let path = RepoPath::parse(path).unwrap();
    let resolved = resolve_repo(files, &path, &RunFlags::default(), &Ceilings::local()).unwrap();
    resolved
        .rules_for(&path)
        .into_iter()
        .map(|r| r.id.unwrap())
        .collect()
}

#[test]
fn nested_scope_is_relative_to_its_folder() {
    let files = files(vec![
        folder(
            None,
            r#"
[[rules]]
id = "root-db"
rule = "Use the pool."
scope = ["src/db/**"]

[[rules]]
id = "everywhere"
rule = "No unwrap."
"#,
        ),
        folder(
            Some("pkg"),
            r#"
[[rules]]
id = "pkg-db"
rule = "Use the pkg pool."
scope = ["src/db/**"]
"#,
        ),
    ]);
    assert_eq!(
        rule_ids(&files, "src/db/conn.rs"),
        ["root-db", "everywhere"]
    );
    // `src/db/**` in pkg/.gitcko/ means pkg/src/db/**, not the root's src/db/.
    assert_eq!(
        rule_ids(&files, "pkg/src/db/conn.rs"),
        ["everywhere", "pkg-db"]
    );
    assert_eq!(rule_ids(&files, "pkg/src/api.rs"), ["everywhere"]);
}

#[test]
fn a_single_star_stays_inside_one_folder() {
    let files = files(vec![folder(
        None,
        r#"
[[rules]]
id = "top"
rule = "Top-level only."
scope = ["src/*.rs"]
"#,
    )]);
    assert_eq!(rule_ids(&files, "src/lib.rs"), ["top"]);
    assert!(rule_ids(&files, "src/db/conn.rs").is_empty());
}

#[test]
fn context_files_follow_scope_and_get_repo_relative_paths() {
    let files = files(vec![folder(
        Some("api"),
        r#"
[[context.files]]
path = "schema.sql"
scope = ["**/*.sql", "src/db/**"]

[[context.files]]
path = "README.md"
"#,
    )]);
    let reviewed = RepoPath::parse("api/src/db/q.rs").unwrap();
    let resolved =
        resolve_repo(&files, &reviewed, &RunFlags::default(), &Ceilings::local()).unwrap();
    let paths: Vec<String> = resolved
        .context_files_for(&reviewed)
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(paths, ["api/schema.sql", "api/README.md"]);

    let other = RepoPath::parse("api/src/http.rs").unwrap();
    let resolved = resolve_repo(&files, &other, &RunFlags::default(), &Ceilings::local()).unwrap();
    let paths: Vec<String> = resolved
        .context_files_for(&other)
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(paths, ["api/README.md"]);
}

#[test]
fn an_invalid_glob_is_refused_at_load_without_echoing_it() {
    let files = files(vec![folder(
        None,
        r#"
[[rules]]
rule = "x"
scope = ["src/[canary"]
"#,
    )]);
    let path = RepoPath::parse("src/a.rs").unwrap();
    let err = resolve_repo(&files, &path, &RunFlags::default(), &Ceilings::local()).unwrap_err();
    assert_eq!(err.key.as_deref(), Some("rules.scope"));
    assert_eq!(err.line, Some(5));
    assert!(!err.to_string().contains("canary"), "{err}");
}

/// The policy digest (CONFIGURATION §11) for the root config `toml` plus `rules_md`.
fn digest(toml: &str, rules_md: Option<&str>) -> String {
    let mut root = folder(None, toml);
    root.rules_md = rules_md.map(str::to_string);
    let path = RepoPath::parse("src/a.rs").unwrap();
    resolve_repo(
        &files(vec![root]),
        &path,
        &RunFlags::default(),
        &Ceilings::local(),
    )
    .unwrap()
    .policy_digest()
}

#[test]
fn the_policy_digest_changes_with_values_not_with_formatting() {
    let a = digest("strictness = 1\n", Some("Prefer the pool."));
    assert!(a.starts_with("sha256:") && a.len() == 7 + 64, "{a}");
    // Comments, spacing and key order are formatting, not policy.
    assert_eq!(
        a,
        digest("# hi\nstrictness   =   1\n\n", Some("Prefer the pool."))
    );
    assert_ne!(a, digest("strictness = 2\n", Some("Prefer the pool.")));
    assert_ne!(a, digest("strictness = 1\n", Some("Prefer the pool!")));
    assert_ne!(a, digest("strictness = 1\n", None));
}
