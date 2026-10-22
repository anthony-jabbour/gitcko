// SPDX-License-Identifier: AGPL-3.0-or-later

use gitcko_test_support::{TempDir, fixture_repo, fixture_repo_with_inherited_env, git};

/// Fixed author, dates and config make the commit id a constant. If this changes on
/// one OS only, something (autocrlf, a global hook, a template) leaked in.
const SINGLE_COMMIT_HEAD: &str = "1af1edf5b48c2c0a686eb899aea9883968d5ddc4";

fn head(repo: &TempDir) -> String {
    let out = git(repo.path())
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn fixture_repos_are_identical_on_every_machine() {
    assert_eq!(head(&fixture_repo("single_commit")), SINGLE_COMMIT_HEAD);
}

#[test]
fn inherited_git_settings_cannot_change_a_fixture() {
    // A developer's shell can export git config and repository overrides; none of them
    // may reach the script.
    let repo = fixture_repo_with_inherited_env(
        "single_commit",
        &[
            ("GIT_CONFIG_COUNT", "2"),
            ("GIT_CONFIG_KEY_0", "commit.gpgSign"),
            ("GIT_CONFIG_VALUE_0", "true"),
            ("GIT_CONFIG_KEY_1", "core.autocrlf"),
            ("GIT_CONFIG_VALUE_1", "true"),
            ("GIT_CONFIG_PARAMETERS", "'user.name'='Someone Else'"),
            ("GIT_DEFAULT_HASH", "sha256"),
            ("GIT_AUTHOR_NAME", "Someone Else"),
        ],
    );
    assert_eq!(head(&repo), SINGLE_COMMIT_HEAD);
}

#[test]
fn temp_dirs_are_private_and_never_reused() {
    let a = TempDir::new("same-label");
    let b = TempDir::new("same-label");
    assert_ne!(a.path(), b.path());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(a.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "other users can reach {:o}", mode);
    }
}

#[test]
fn temp_dirs_are_removed_on_drop() {
    let dir = TempDir::new("drop-check");
    let path = dir.path().to_path_buf();
    std::fs::write(path.join("x"), "x").unwrap();
    drop(dir);
    assert!(!path.exists());
}

#[test]
#[should_panic(expected = "make_no_such_fixture.sh")]
fn missing_fixture_script_fails_loudly() {
    fixture_repo("no_such_fixture");
}

#[test]
fn git_helper_reads_no_user_or_system_config() {
    // Only the repository's own config may apply, or output (e.g. path quoting) would
    // differ between developers' machines.
    let repo = fixture_repo("single_commit");
    let out = git(repo.path())
        .args(["config", "--list", "--show-scope"])
        .output()
        .unwrap();
    // A failing git call prints nothing, which would otherwise look like a pass.
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = String::from_utf8_lossy(&out.stdout);
    let foreign: Vec<_> = listed.lines().filter(|l| !l.starts_with("local")).collect();
    assert!(foreign.is_empty(), "{foreign:?}");
}
