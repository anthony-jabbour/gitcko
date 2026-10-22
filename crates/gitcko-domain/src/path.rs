// SPDX-License-Identifier: AGPL-3.0-or-later

//! Repository-relative paths that can't point outside the repository.

use std::fmt;

use gix_validate::path::component;
use gix_validate::path::component::Options;

/// A path inside a repository: `/`-separated, relative, with no `.` or `..` part, no
/// control characters and no `.git` under any alias. It is the only path type that
/// crosses crate boundaries, so every path from a tree, a diff, a config file or a
/// model is checked once, on the way in (SECURITY §14.7, THREAT_MODEL TM-19).
///
/// The rules match what git itself accepts in a tree on every OS (`core.protectNTFS`
/// and `core.protectHFS`, on by default since git 2.24), checked with gitoxide's
/// `gix-validate`. Names that are only a problem on Windows, like `CON` or `a\b`, are
/// legal files in a Linux repo, so they parse; [`RepoPath::windows_problem`] reports
/// them for code that is about to touch the disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RepoPath(String);

/// Why a string isn't a [`RepoPath`]. Shows the offending path, which is
/// repository-relative and never an absolute local path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid repository path {path:?}: {problem}")]
pub struct PathError {
    pub path: String,
    pub problem: PathProblem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathProblem {
    #[error("it is empty")]
    Empty,
    #[error("it is absolute")]
    Absolute,
    #[error("it has an empty part (`//` or a trailing `/`)")]
    EmptyComponent,
    #[error("`.` and `..` parts are not allowed")]
    Relative,
    #[error("control characters are not allowed")]
    ControlCharacter,
    #[error("`.git` (or a name Windows or macOS treat as `.git`) is not allowed")]
    DotGit,
}

/// Git's own checks, minus the Windows-only ones (see [`RepoPath`]).
const EVERY_OS: Options = Options {
    protect_windows: false,
    protect_hfs: true,
    protect_ntfs: true,
};

impl RepoPath {
    pub fn parse(path: &str) -> Result<RepoPath, PathError> {
        let fail = |problem| {
            Err(PathError {
                path: path.to_string(),
                problem,
            })
        };
        if path.is_empty() {
            return fail(PathProblem::Empty);
        }
        if path.chars().any(char::is_control) {
            return fail(PathProblem::ControlCharacter);
        }
        if path.starts_with('/') {
            return fail(PathProblem::Absolute);
        }
        for part in path.split('/') {
            if part.is_empty() {
                return fail(PathProblem::EmptyComponent);
            }
            if part == "." || part == ".." {
                return fail(PathProblem::Relative);
            }
            // Only `.git` aliases are left for gix-validate to find with these options.
            if component(part.as_bytes().into(), None, EVERY_OS).is_err() {
                return fail(PathProblem::DotGit);
            }
        }
        Ok(RepoPath(path.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Why opening this path on a Windows disk would be unsafe or open the wrong thing
    /// (`CON`, `C:`, `a\b`, a trailing dot, `:` or `?`), or `None`. Checked on every OS,
    /// so a repo that can't be read safely on Windows is noticed on macOS too.
    pub fn windows_problem(&self) -> Option<String> {
        let windows = Options {
            protect_windows: true,
            ..EVERY_OS
        };
        self.0.split('/').find_map(|part| {
            component(part.as_bytes().into(), None, windows)
                .err()
                .map(|e| format!("{part:?}: {e}"))
        })
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for RepoPath {
    type Err = PathError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RepoPath::parse(s)
    }
}
