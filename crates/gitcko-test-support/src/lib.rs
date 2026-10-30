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
    run_fixture_script(&script, name, inherited)
}

/// A repository built by any fixture script, with the same fixed identity, dates and
/// empty git config as [`fixture_repo`]. For the review eval cases, which live beside
/// their expected results rather than in `tests/fixtures/repos/`.
pub fn script_repo(script: &Path, label: &str) -> TempDir {
    run_fixture_script(script, label, &[])
}

fn run_fixture_script(script: &Path, name: &str, inherited: &[(&str, &str)]) -> TempDir {
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
        .arg(script)
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

/// An empty repository on branch `main`, for tests that build commits with
/// [`literal_commit`].
pub fn empty_repo(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    run_git(dir.path(), &["init", "-q", "-b", "main"], None);
    dir
}

/// Writes a commit whose tree holds exactly `files` (path bytes, mode, content) and
/// returns its id. The objects are written directly (`hash-object --literally`), never
/// checked out, so a test can build trees no filesystem allows: `..` or `.git` parts,
/// `CON`, names that differ only in case, non-UTF-8 names, symlinks to anywhere. For
/// mode `160000` (a submodule) `content` is the 40-hex commit id it points at.
pub fn literal_commit(repo: &Path, files: &[(&[u8], &str, &[u8])]) -> String {
    let entries: Vec<LiteralFile> = files
        .iter()
        .map(|(path, mode, content)| {
            let parts = path.split(|b| *b == b'/').map(<[u8]>::to_vec).collect();
            let oid = if *mode == "160000" {
                std::str::from_utf8(content).unwrap().to_string()
            } else {
                write_object(repo, "blob", content)
            };
            (parts, *mode, hex_to_bytes(&oid))
        })
        .collect();
    let tree = write_tree(repo, &entries);
    let commit = run_git(
        repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@fixture.invalid",
            "commit-tree",
            &tree,
            "-m",
            "literal",
        ],
        None,
    );
    commit.trim().to_string()
}

/// A file on its way into a literal tree: path parts, mode, object id bytes.
type LiteralFile<'a> = (Vec<Vec<u8>>, &'a str, Vec<u8>);

/// One tree level: files here, folders recursed into. Entries are sorted the way git
/// sorts them, comparing a folder's name as if it ended in `/`, or git calls the tree
/// corrupt.
fn write_tree(repo: &Path, entries: &[LiteralFile]) -> String {
    let mut here: Vec<(Vec<u8>, String, Vec<u8>)> = Vec::new();
    let mut folders: std::collections::BTreeMap<Vec<u8>, Vec<LiteralFile>> = Default::default();
    for (parts, mode, oid) in entries {
        if parts.len() == 1 {
            here.push((parts[0].clone(), (*mode).to_string(), oid.clone()));
        } else {
            folders.entry(parts[0].clone()).or_default().push((
                parts[1..].to_vec(),
                mode,
                oid.clone(),
            ));
        }
    }
    for (name, children) in folders {
        let oid = hex_to_bytes(&write_tree(repo, &children));
        here.push((name, "40000".into(), oid));
    }
    let sort_key = |(name, mode, _): &(Vec<u8>, String, Vec<u8>)| {
        let mut key = name.clone();
        if mode == "40000" {
            key.push(b'/');
        }
        key
    };
    here.sort_by_key(sort_key);
    let mut bytes = Vec::new();
    for (name, mode, oid) in here {
        bytes.extend(mode.as_bytes());
        bytes.push(b' ');
        bytes.extend(name);
        bytes.push(0);
        bytes.extend(oid);
    }
    write_object(repo, "tree", &bytes)
}

fn write_object(repo: &Path, kind: &str, content: &[u8]) -> String {
    let args = ["hash-object", "-t", kind, "--literally", "-w", "--stdin"];
    run_git(repo, &args, Some(content)).trim().to_string()
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

/// Runs git through [`git`], feeding `stdin`, and returns stdout; panics with stderr on
/// failure. Dates are fixed so the same objects give the same ids on every machine.
fn run_git(dir: &Path, args: &[&str], stdin: Option<&[u8]>) -> String {
    use std::io::Write;
    let date = "2026-01-01T00:00:00Z";
    let mut child = git(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.unwrap_or_default())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
