// SPDX-License-Identifier: AGPL-3.0-or-later

//! One golden per precedence and merge rule (CONFIGURATION §2, §3, §11). Each case in
//! `tests/fixtures/config/<case>/` is a small `repo/` tree plus `case.toml`: the file
//! under review, the user file, env vars, run flags, or the error expected.

use std::collections::BTreeMap;
use std::path::Path;

use gitcko_config::{Ceilings, FolderConfig, RepoFiles, RunFlags, resolve_repo, resolve_user};
use gitcko_domain::RepoPath;

#[derive(serde::Deserialize)]
struct Case {
    reviewed: String,
    user: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    flags: toml::Table,
    expect_error: Option<String>,
}

/// Every `.gitcko/` folder under `repo/`, as a caller reading the repository would pass
/// them in.
fn repo_files(repo: &Path) -> RepoFiles {
    let mut folders = Vec::new();
    collect(repo, repo, &mut folders);
    folders.sort_by(|a: &FolderConfig, b| a.folder.cmp(&b.folder));
    RepoFiles {
        folders,
        legacy_root_file: repo.join(".gitcko.toml").exists(),
    }
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<FolderConfig>) {
    let gitcko = dir.join(".gitcko");
    if gitcko.is_dir() {
        let relative = dir
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        out.push(FolderConfig {
            folder: (!relative.is_empty()).then(|| RepoPath::parse(&relative).unwrap()),
            config_toml: std::fs::read_to_string(gitcko.join("config.toml")).ok(),
            rules_md: std::fs::read_to_string(gitcko.join("rules.md")).ok(),
        });
    }
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        if entry.path().is_dir() && entry.file_name() != ".gitcko" {
            collect(root, &entry.path(), out);
        }
    }
}

fn run(name: &str, files: RepoFiles) {
    let dir = gitcko_test_support::workspace_root()
        .join("tests/fixtures/config")
        .join(name);
    let case: Case =
        toml::from_str(&std::fs::read_to_string(dir.join("case.toml")).unwrap()).unwrap();
    let flags = RunFlags::from_table(&case.flags).unwrap();
    let reviewed = RepoPath::parse(&case.reviewed).unwrap();
    let repo = resolve_repo(&files, &reviewed, &flags, &Ceilings::local());
    let user = resolve_user(case.user.as_deref(), &case.env, &flags);
    match (&case.expect_error, repo, user) {
        (Some(expected), repo, user) => {
            let errors: Vec<String> = [repo.err(), user.err()]
                .into_iter()
                .flatten()
                .map(|e| e.to_string())
                .collect();
            assert!(
                errors.iter().any(|e| e.contains(expected.as_str())),
                "{name}: expected {expected:?} in {errors:?}"
            );
        }
        (None, Ok(repo), Ok(user)) => {
            insta::assert_snapshot!(
                name,
                format!("# repo\n{}\n# user\n{}", repo.explain(), user.explain())
            );
        }
        (None, repo, user) => panic!("{name}: {:?} {:?}", repo.err(), user.err()),
    }
}

#[test]
fn every_config_case() {
    let root = gitcko_test_support::workspace_root().join("tests/fixtures/config");
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(names.len() >= 16, "{names:?}");
    for name in names {
        if name == "secret_in_file" {
            continue; // built in its own test, from a canary
        }
        run(&name, repo_files(&root.join(&name).join("repo")));
    }
}

#[test]
fn canary_secret_in_a_config_file_is_an_error_that_never_shows_it() {
    for canary in gitcko_redact::canaries() {
        let text = format!(
            "schema_version = 1\ninstructions = {}\n",
            toml::Value::String(canary.clone())
        );
        let files = RepoFiles {
            folders: vec![FolderConfig {
                folder: None,
                config_toml: Some(text),
                rules_md: None,
            }],
            legacy_root_file: false,
        };
        let reviewed = RepoPath::parse("a.txt").unwrap();
        let err = resolve_repo(&files, &reviewed, &RunFlags::default(), &Ceilings::local())
            .expect_err("a secret in a repo file is an error");
        let shown = format!("{err} {err:?}");
        assert!(
            shown.contains(".gitcko/config.toml:2: instructions: looks like a secret"),
            "{shown}"
        );
        assert!(!shown.contains(canary.as_str()), "{shown}");
    }
}
