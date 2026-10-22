// SPDX-License-Identifier: AGPL-3.0-or-later

//! Helpers shared by every crate's tests. Not published: nothing here ships in Gitcko.
//!
//! Tests never touch the network or real code; fixtures are synthetic and live under
//! `tests/fixtures/` at the workspace root (see its README).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// The workspace root, so tests in any crate find `tests/fixtures/`.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A directory under the system temp folder that is deleted when dropped.
/// ponytail: std only; reach for the `tempfile` crate if tests need more than this.
pub struct TempDir {
    /// Deleted on drop.
    root: PathBuf,
    /// What `path()` returns: the root, or a folder inside it.
    path: PathBuf,
}

impl TempDir {
    /// `label` makes leftovers easy to trace if a test process is killed.
    pub fn new(label: &str) -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("gitcko-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        TempDir {
            root: dir.clone(),
            path: dir,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Builds the fixture repo `tests/fixtures/repos/make_<name>.sh` in a fresh temp dir and
/// returns it. The script runs with its working directory set to the empty repo folder.
///
/// Fixed author, committer and dates, and no system or global git config, so the same
/// script gives the same commit ids on every machine and OS. Building per test instead
/// of unpacking cached archives (the gitoxide pattern) is fine while repos are tiny.
pub fn fixture_repo(name: &str) -> TempDir {
    let script = workspace_root().join(format!("tests/fixtures/repos/make_{name}.sh"));
    assert!(
        script.is_file(),
        "fixture script {} not found",
        script.display()
    );
    let mut dir = TempDir::new(&format!("repo-{name}"));
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let empty_config = dir.path().join("gitconfig");
    std::fs::write(&empty_config, "").unwrap();

    let date = "2026-01-01T00:00:00Z";
    let out = Command::new(bash())
        .arg(&script)
        .current_dir(&repo)
        .env("HOME", dir.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &empty_config)
        .env("GIT_AUTHOR_NAME", "Fixture Author")
        .env("GIT_AUTHOR_EMAIL", "author@fixture.invalid")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_NAME", "Fixture Committer")
        .env("GIT_COMMITTER_EMAIL", "committer@fixture.invalid")
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", script.display()));
    assert!(
        out.status.success(),
        "{} failed:\n{}",
        script.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    // Hand back the repo folder; dropping it still removes the throwaway config beside it.
    dir.path = repo;
    dir
}

/// Git for Windows' bash. On Windows `bash` on the PATH can be WSL's, which can't run
/// our scripts against a Windows folder, so find the one next to `git` instead (the
/// same lookup gitoxide's test tools do).
fn bash() -> PathBuf {
    if cfg!(windows) {
        let exec_path = Command::new("git").arg("--exec-path").output().ok();
        if let Some(out) = exec_path.filter(|o| o.status.success()) {
            // e.g. C:/Program Files/Git/mingw64/libexec/git-core → C:/Program Files/Git
            let exec_path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            if let Some(git_root) = exec_path.ancestors().nth(3) {
                let candidate = git_root.join("bin").join("bash.exe");
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    PathBuf::from("bash")
}

/// One row of `tests/fixtures/windows/paths.toml`.
#[derive(Debug, serde::Deserialize)]
pub struct PathCase {
    pub path: String,
    /// Accepted as a repository path on every OS.
    pub valid: bool,
    /// Also safe to open on Windows.
    pub windows_safe: bool,
    pub why: String,
}

/// The Windows path cases, kept as data because many can't be committed as real files.
pub fn windows_path_cases() -> Vec<PathCase> {
    #[derive(serde::Deserialize)]
    struct File {
        case: Vec<PathCase>,
    }
    let path = workspace_root().join("tests/fixtures/windows/paths.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    toml::from_str::<File>(&text)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .case
}
