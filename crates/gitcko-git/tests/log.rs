// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Repo::log`, cross-checked with `git log --first-parent`.

use std::path::Path;

use gitcko_domain::RepoPath;
use gitcko_git::Repo;
use gitcko_test_support::{TempDir, empty_repo, git};

fn run(dir: &Path, args: &[&str]) -> String {
    let out = git(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// Four commits: a.rs, b.rs, a.rs again, then b.rs.
fn history() -> TempDir {
    let dir = empty_repo("log");
    for (i, file) in ["a.rs", "b.rs", "a.rs", "b.rs"].iter().enumerate() {
        std::fs::write(dir.path().join(file), format!("{i}\n")).unwrap();
        run(dir.path(), &["add", file]);
        run(
            dir.path(),
            &[
                "commit",
                "-q",
                "-m",
                &format!("change {i} to {file}\n\nbody"),
            ],
        );
    }
    dir
}

fn ids(repo: &Repo, path: Option<&str>, limit: usize) -> Vec<String> {
    let head = repo.resolve("HEAD").unwrap();
    let path = path.map(|p| RepoPath::parse(p).unwrap());
    repo.log(head, path.as_ref(), limit, 100)
        .unwrap()
        .entries
        .iter()
        .map(|e| e.id.to_string())
        .collect()
}

#[test]
fn matches_git_log_with_and_without_a_path() {
    let dir = history();
    let repo = Repo::open(dir.path()).unwrap();
    for path in [None, Some("a.rs"), Some("b.rs"), Some("missing.rs")] {
        let mut args = vec!["log", "--first-parent", "--format=%H"];
        if let Some(p) = path {
            args.extend(["--", p]);
        }
        let expected: Vec<String> = run(dir.path(), &args).lines().map(str::to_string).collect();
        assert_eq!(ids(&repo, path, 10), expected, "{path:?}");
    }
    let log = repo
        .log(repo.resolve("HEAD").unwrap(), None, 10, 100)
        .unwrap();
    assert_eq!(log.entries[0].summary, "change 3 to b.rs");
    assert!(!log.truncated && !log.shallow);
}

#[test]
fn the_limit_and_scan_budget_say_truncated() {
    let dir = history();
    let repo = Repo::open(dir.path()).unwrap();
    let head = repo.resolve("HEAD").unwrap();
    let log = repo.log(head, None, 2, 100).unwrap();
    assert_eq!((log.entries.len(), log.truncated), (2, true));
    // Two commits scanned can't reach a.rs's newest change at commit 3 of 4.
    let a = RepoPath::parse("a.rs").unwrap();
    let log = repo.log(head, Some(&a), 10, 1).unwrap();
    assert_eq!((log.entries.len(), log.truncated), (0, true));
}

#[test]
fn a_shallow_clone_is_reported_not_passed_off_as_the_whole_history() {
    let dir = history();
    let clone = TempDir::new("log-shallow");
    // `--depth` needs a URL, not a path; on Windows the path is `C:\...`.
    let local = dir.path().display().to_string().replace('\\', "/");
    let url = if local.starts_with('/') {
        format!("file://{local}")
    } else {
        format!("file:///{local}")
    };
    let out = git(clone.path())
        .args(["clone", "-q", "--depth", "2", &url, "."])
        .output()
        .unwrap();
    assert!(out.status.success());
    let repo = Repo::open(clone.path()).unwrap();
    let log = repo
        .log(repo.resolve("HEAD").unwrap(), None, 10, 100)
        .unwrap();
    assert_eq!(log.entries.len(), 2);
    assert!(log.shallow && !log.truncated);
    // The oldest commit here changed a.rs; its parent is missing, so it counts as a
    // change rather than failing to read the parent.
    let a = RepoPath::parse("a.rs").unwrap();
    let log = repo
        .log(repo.resolve("HEAD").unwrap(), Some(&a), 10, 100)
        .unwrap();
    assert_eq!(log.entries.len(), 1);
    assert!(log.shallow);
}

#[test]
fn a_long_subject_is_cut_to_200_characters() {
    let dir = empty_repo("log-long");
    std::fs::write(dir.path().join("a"), "x").unwrap();
    run(dir.path(), &["add", "a"]);
    run(dir.path(), &["commit", "-q", "-m", &"é".repeat(300)]);
    let repo = Repo::open(dir.path()).unwrap();
    let log = repo.log(repo.resolve("HEAD").unwrap(), None, 1, 1).unwrap();
    assert_eq!(log.entries[0].summary, "é".repeat(200));
}
