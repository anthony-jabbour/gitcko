// SPDX-License-Identifier: AGPL-3.0-or-later

//! gitcko-git against fixture repos, cross-checked with the real `git` CLI.

use std::path::Path;

use gitcko_diff::{Change, ChangeSet, parse};
use gitcko_domain::RepoPath;
use gitcko_git::Repo;
use gitcko_test_support::{TempDir, fixture_repo, git};

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = git(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn path(p: &str) -> RepoPath {
    RepoPath::parse(p).unwrap()
}

/// git and gix score rename similarity differently (bytes vs lines), so compare
/// everything else.
fn without_similarity(mut changes: ChangeSet) -> ChangeSet {
    for file in &mut changes.files {
        if let Change::Renamed { .. } = file.change {
            file.change = Change::Renamed { similarity: 0 };
        }
    }
    changes.files.sort_by(|a, b| {
        let key = |f: &gitcko_diff::FileDiff| f.new_path.clone().or(f.old_path.clone());
        key(a).cmp(&key(b))
    });
    changes
}

#[test]
fn resolves_branches_and_ids_to_commits() {
    let fixture = fixture_repo("linear_edits");
    let repo = Repo::open(fixture.path()).unwrap();
    let head = git_out(fixture.path(), &["rev-parse", "main"]);
    assert_eq!(repo.resolve("main").unwrap().to_string(), head.trim());
    assert_eq!(repo.resolve("HEAD").unwrap().to_string(), head.trim());
    assert_eq!(repo.resolve(head.trim()).unwrap().to_string(), head.trim());
    let err = repo.resolve("no-such-branch").unwrap_err();
    assert!(err.to_string().contains("no-such-branch"), "{err}");
}

#[test]
fn diff_matches_the_git_cli() {
    let fixture = fixture_repo("linear_edits");
    let repo = Repo::open(fixture.path()).unwrap();
    let (base, head) = (repo.resolve("base").unwrap(), repo.resolve("main").unwrap());
    let ours = repo.diff(base, head).unwrap();
    let theirs = parse(&git_out(fixture.path(), &["diff", "-M", "base", "main"])).unwrap();
    assert_eq!(without_similarity(ours.clone()), without_similarity(theirs));
    let renamed: Vec<_> = ours
        .files
        .iter()
        .filter(|f| matches!(f.change, Change::Renamed { .. }))
        .map(|f| f.new_path.as_ref().unwrap().as_str())
        .collect();
    assert_eq!(renamed.len(), 2, "{renamed:?}");
}

#[test]
fn merge_base_agrees_with_git() {
    let fixture = fixture_repo("merge_bases");
    let repo = Repo::open(fixture.path()).unwrap();
    let id = |rev: &str| repo.resolve(rev).unwrap();

    let simple = repo.merge_base(id("left"), id("main")).unwrap().unwrap();
    assert_eq!(
        simple.to_string(),
        git_out(fixture.path(), &["merge-base", "left", "main"]).trim()
    );

    // A criss-cross merge has two equally good bases; either is a correct answer.
    let cc = repo.merge_base(id("cc_a"), id("cc_b")).unwrap().unwrap();
    let all = git_out(fixture.path(), &["merge-base", "--all", "cc_a", "cc_b"]);
    assert_eq!(all.lines().count(), 2, "fixture should have two bases");
    assert!(
        all.lines().any(|l| l == cc.to_string()),
        "{cc} not in {all}"
    );

    assert_eq!(repo.merge_base(id("lonely"), id("main")).unwrap(), None);
}

#[test]
fn reads_files_from_a_commit_without_following_symlinks() {
    let fixture = fixture_repo("linear_edits");
    let repo = Repo::open(fixture.path()).unwrap();
    let head = repo.resolve("main").unwrap();
    let file = repo
        .read_blob(head, &path("sub/moved.txt"))
        .unwrap()
        .unwrap();
    assert_eq!(file.bytes, b"alpha\nbeta\nGAMMA\ndelta\nepsilon\n");
    assert!(!file.symlink);
    let link = repo.read_blob(head, &path("link.txt")).unwrap().unwrap();
    assert!(link.symlink);
    assert_eq!(
        link.bytes, b"numbers.txt",
        "the target, not the target's content"
    );
    assert!(
        repo.read_blob(head, &path("no/such/file"))
            .unwrap()
            .is_none()
    );
    assert!(
        repo.read_blob(head, &path("sub")).unwrap().is_none(),
        "a folder isn't a file"
    );
}

#[test]
fn finds_case_only_collisions() {
    // Two files a case-insensitive disk (Windows, default macOS) can't hold at once.
    let fixture = fixture_repo("linear_edits");
    let repo = Repo::open(fixture.path()).unwrap();
    let collisions = repo.case_collisions(repo.resolve("main").unwrap()).unwrap();
    assert_eq!(collisions, vec![(path("README.md"), path("readme.md"))]);
    assert!(
        repo.case_collisions(repo.resolve("base").unwrap())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn never_runs_hooks_filters_or_diff_drivers() {
    let fixture = fixture_repo("hostile");
    let marker = fixture.path().parent().unwrap().join("PWNED");

    // Prove the fixture is armed: real git runs the textconv driver on this diff.
    git_out(fixture.path(), &["diff", "HEAD~1", "HEAD"]);
    assert!(
        marker.exists(),
        "fixture didn't fire with real git; the test would prove nothing"
    );
    std::fs::remove_file(&marker).unwrap();

    let repo = Repo::open(fixture.path()).unwrap();
    let (base, head) = (
        repo.resolve("HEAD~1").unwrap(),
        repo.resolve("HEAD").unwrap(),
    );
    let changes = repo.diff(base, head).unwrap();
    assert_eq!(changes.files.len(), 1);
    repo.merge_base(base, head).unwrap();
    repo.read_blob(head, &path("file.txt")).unwrap().unwrap();
    repo.case_collisions(head).unwrap();
    repo.read_worktree_file(&path("file.txt")).unwrap();
    assert!(
        !marker.exists(),
        "gitcko-git ran a repository-controlled command"
    );
}

#[test]
fn worktree_reads_stay_inside_the_repository() {
    let fixture = fixture_repo("single_commit");
    let repo = Repo::open(fixture.path()).unwrap();
    assert_eq!(
        repo.read_worktree_file(&path("README.md")).unwrap(),
        b"hello\n"
    );

    let outside = TempDir::new("outside");
    std::fs::write(outside.path().join("secret.txt"), "outside").unwrap();
    link_dir(outside.path(), &fixture.path().join("escape"));
    let err = repo
        .read_worktree_file(&path("escape/secret.txt"))
        .unwrap_err();
    assert!(err.to_string().contains("outside the repository"), "{err}");

    // A link that stays inside the repository is fine.
    std::fs::create_dir(fixture.path().join("docs")).unwrap();
    std::fs::write(fixture.path().join("docs/a.txt"), "inside").unwrap();
    link_dir(
        &fixture.path().join("docs"),
        &fixture.path().join("docs-link"),
    );
    assert_eq!(
        repo.read_worktree_file(&path("docs-link/a.txt")).unwrap(),
        b"inside"
    );
}

/// A directory link: a symlink on Unix, a junction on Windows (no admin rights needed,
/// and the case TESTING §8 names).
fn link_dir(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .unwrap();
        assert!(status.success(), "mklink /J failed");
    }
}

#[cfg(unix)]
#[test]
fn worktree_reads_refuse_special_files() {
    let fixture = fixture_repo("single_commit");
    let repo = Repo::open(fixture.path()).unwrap();
    let fifo = fixture.path().join("pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());
    // Opening a FIFO would block forever; it must be refused before that.
    let err = repo.read_worktree_file(&path("pipe")).unwrap_err();
    assert!(err.to_string().contains("not a regular file"), "{err}");
}

#[cfg(windows)]
#[test]
fn worktree_reads_refuse_windows_device_names() {
    let fixture = fixture_repo("single_commit");
    let repo = Repo::open(fixture.path()).unwrap();
    let err = repo.read_worktree_file(&path("CON")).unwrap_err();
    assert!(err.to_string().contains("Windows"), "{err}");
}

#[test]
fn canary_errors_hold_no_local_paths() {
    // OBSERVABILITY §6: errors may reach logs, so they name repository paths only.
    let dir = TempDir::new("not-a-repo");
    let err = Repo::open(dir.path())
        .err()
        .expect("an empty folder isn't a repository");
    let shown = format!("{err} {err:?}");
    let local = dir.path().to_string_lossy();
    assert!(!shown.contains(local.as_ref()), "{shown}");
}
