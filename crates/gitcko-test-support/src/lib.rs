// SPDX-License-Identifier: AGPL-3.0-or-later

//! Helpers shared by every crate's tests. Not published: nothing here ships in Gitcko.
//!
//! Tests never touch the network or real code; fixtures are synthetic and live under
//! `tests/fixtures/` at the workspace root (see its README).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace root, so tests in any crate find `tests/fixtures/`.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A private directory under the system temp folder, deleted when dropped.
///
/// Created exclusively, with a random name and owner-only permissions (via `tempfile`),
/// so another user on a shared machine can't plant files or symlinks in it first.
pub struct TempDir {
    /// Never read: owning it is what deletes the folder on drop.
    _root: tempfile::TempDir,
    /// What `path()` returns: the root, or a folder inside it.
    path: PathBuf,
}

impl TempDir {
    /// `label` makes leftovers easy to trace if a test process is killed.
    pub fn new(label: &str) -> TempDir {
        let prefix = format!("gitcko-{label}-");
        let mut builder = tempfile::Builder::new();
        builder.prefix(&prefix);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let root = builder
            .tempdir()
            .unwrap_or_else(|e| panic!("cannot create a temp dir: {e}"));
        let path = root.path().to_path_buf();
        TempDir { _root: root, path }
    }

    /// The directory as created, not canonicalized. On macOS it sits under `/var`, a
    /// link to `/private/var`, so canonicalize it before comparing it with a path that
    /// was.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Builds the fixture repo `tests/fixtures/repos/make_<name>.sh` in a fresh temp dir and
/// returns it. The script runs with its working directory set to the empty repo folder.
///
/// Fixed author, committer and dates, and no system or global git config, so the same
/// script gives the same commit ids on every machine and OS. Building per test instead
/// of unpacking cached archives (the gitoxide pattern) is fine while repos are tiny.
pub fn fixture_repo(name: &str) -> TempDir {
    fixture_repo_with_inherited_env(name, &[])
}

/// [`fixture_repo`] as if the test process had also inherited `inherited` from its
/// environment. Only for proving that inherited git settings can't leak in.
#[doc(hidden)]
pub fn fixture_repo_with_inherited_env(name: &str, inherited: &[(&str, &str)]) -> TempDir {
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
    let templates = dir.path().join("templates");
    std::fs::create_dir(&templates).unwrap();

    let mut command = Command::new(bash());
    command.envs(inherited.iter().copied());
    without_inherited_git_env(&mut command, inherited.iter().map(|(k, _)| (*k).into()));
    let out = command
        .arg(&script)
        // No hooks or other files copied in by `git init` from a template folder.
        .env("GIT_TEMPLATE_DIR", &templates)
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

/// `git` run in `dir`, for tests that inspect a fixture repo, with the same clean
/// environment the fixture scripts get.
pub fn git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    // Strip inherited settings first, so they can't cancel the ones set below.
    without_inherited_git_env(&mut command, std::iter::empty());
    let no_file = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", no_file);
    command
}

/// Removes every `GIT_*` variable the process inherited (config injection such as
/// `GIT_CONFIG_COUNT`/`GIT_CONFIG_PARAMETERS`, repository and object-format overrides,
/// identities), plus `extra` names, so only what a helper sets explicitly applies.
/// Callers set their own `GIT_*` values after this.
fn without_inherited_git_env(
    command: &mut Command,
    extra: impl Iterator<Item = std::ffi::OsString>,
) {
    let inherited = std::env::vars_os().map(|(key, _)| key);
    for key in inherited.chain(extra) {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
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
