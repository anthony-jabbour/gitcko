// SPDX-License-Identifier: AGPL-3.0-or-later

use std::process::Command;

#[test]
fn fixture_repos_are_identical_on_every_machine() {
    // Fixed author, dates and config make the commit id a constant. If this changes on
    // one OS only, something (autocrlf, a global hook, a template) leaked in.
    let repo = gitcko_test_support::fixture_repo("single_commit");
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(head.status.success());
    assert_eq!(
        String::from_utf8(head.stdout).unwrap().trim(),
        "1af1edf5b48c2c0a686eb899aea9883968d5ddc4"
    );
}

#[test]
fn temp_dirs_are_removed_on_drop() {
    let dir = gitcko_test_support::TempDir::new("drop-check");
    let path = dir.path().to_path_buf();
    std::fs::write(path.join("x"), "x").unwrap();
    drop(dir);
    assert!(!path.exists());
}

#[test]
#[should_panic(expected = "make_no_such_fixture.sh")]
fn missing_fixture_script_fails_loudly() {
    gitcko_test_support::fixture_repo("no_such_fixture");
}
