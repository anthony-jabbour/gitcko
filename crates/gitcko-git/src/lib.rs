// SPDX-License-Identifier: AGPL-3.0-or-later

//! Read-only access to the repository under review, through gitoxide (`gix`).
//!
//! Everything reads git's object database, never the checked-out files, except
//! [`Repo::read_worktree_file`], which exists for reviewing uncommitted changes and is
//! guarded against escaping the repository.
//!
//! **Nothing here runs a command.** gix is a pure-Rust git library with no hook
//! runner, and blobs are diffed as stored, so clean/smudge filters, textconv drivers
//! and fsmonitor never run (SECURITY §14.2–14.3, ADR-9). The repository is opened
//! isolated: only its own `.git/config` is read, not the user's or the system's.

mod diff;

use std::path::{Path, PathBuf};

use gitcko_diff::ChangeSet;
use gitcko_domain::{PathError, RepoPath};

/// What went wrong. Messages name repository paths and revisions only, never a local
/// filesystem path, because errors can reach logs (OBSERVABILITY §6).
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("not a git repository, or it can't be opened")]
    Open,
    #[error("can't resolve {rev:?} to a commit")]
    Resolve { rev: String },
    #[error("can't read git object {id}: missing or corrupt")]
    Object { id: String },
    #[error("the repository has no working tree")]
    NoWorktree,
    #[error("{path}: resolves outside the repository")]
    Escape { path: RepoPath },
    #[error("{path}: resolves into git metadata")]
    GitMetadata { path: RepoPath },
    #[error("{path}: not a regular file")]
    NotRegularFile { path: RepoPath },
    #[error("{path}: unsafe to open on Windows ({problem})")]
    WindowsUnsafe { path: RepoPath, problem: String },
    #[error("{path}: {reason}")]
    Read { path: RepoPath, reason: String },
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("a path in the repository isn't UTF-8")]
    NonUtf8Path,
}

pub type Result<T, E = GitError> = std::result::Result<T, E>;

/// A commit id (the full hex hash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommitId(gix::ObjectId);

impl std::fmt::Display for CommitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A file's content at a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub bytes: Vec<u8>,
    /// A symbolic link: `bytes` is the link's target, which is never followed.
    pub symlink: bool,
}

pub struct Repo {
    repo: gix::Repository,
    /// The working tree, canonical, when there is one.
    workdir: Option<PathBuf>,
    /// The git dir, canonical: never readable through the working tree.
    git_dir: Option<PathBuf>,
}

/// gix's error text can include local paths (a loose object's file, for one), so only
/// the object id goes into ours; the gix error itself is dropped.
fn object_error(id: impl std::fmt::Display, _err: impl std::fmt::Display) -> GitError {
    GitError::Object { id: id.to_string() }
}

impl Repo {
    pub fn open(path: &Path) -> Result<Repo> {
        let repo =
            gix::open_opts(path, gix::open::Options::isolated()).map_err(|_| GitError::Open)?;
        let workdir = repo.workdir().and_then(|dir| dir.canonicalize().ok());
        let git_dir = repo.git_dir().canonicalize().ok();
        Ok(Repo {
            repo,
            workdir,
            git_dir,
        })
    }

    /// Resolves a branch, tag, `HEAD`, `HEAD~1` or hash to the commit it names, so a
    /// review is bound to an immutable id, not a moving branch (ARCHITECTURE §5.2).
    pub fn resolve(&self, rev: &str) -> Result<CommitId> {
        let err = || GitError::Resolve {
            rev: rev.to_string(),
        };
        let id = self.repo.rev_parse_single(rev).map_err(|_| err())?;
        let commit = id
            .object()
            .map_err(|_| err())?
            .peel_to_commit()
            .map_err(|_| err())?;
        Ok(CommitId(commit.id))
    }

    /// The best common ancestor of two commits, or `None` if their histories never
    /// meet. With several equally good bases (a criss-cross merge) it returns one of
    /// them, like `git merge-base`.
    pub fn merge_base(&self, one: CommitId, two: CommitId) -> Result<Option<CommitId>> {
        match self.repo.merge_base(one.0, two.0) {
            Ok(base) => Ok(base.map(|id| CommitId(id.detach()))),
            Err(err) => Err(object_error(format!("{one}..{two}"), err)),
        }
    }

    /// What changed from `base` to `head`, with renames detected.
    pub fn diff(&self, base: CommitId, head: CommitId) -> Result<ChangeSet> {
        diff::trees(&self.repo, self.tree(base)?, self.tree(head)?)
    }

    fn tree(&self, commit: CommitId) -> Result<gix::Tree<'_>> {
        let commit = self
            .repo
            .find_commit(commit.0)
            .map_err(|e| object_error(commit, e))?;
        commit.tree().map_err(|e| object_error(commit.id, e))
    }

    /// The file at `path` in `commit`, or `None` if there's no file there (missing, or
    /// a folder or submodule).
    pub fn read_blob(&self, commit: CommitId, path: &RepoPath) -> Result<Option<Blob>> {
        // Walk by name rather than as an OS path, so a `\` in a name is never taken for
        // a separator on Windows.
        let mut tree = self.tree(commit)?;
        let mut parts = path.as_str().split('/').peekable();
        while let Some(part) = parts.next() {
            let entry = tree
                .iter()
                .filter_map(|e| e.ok())
                .find(|e| e.filename() == part.as_bytes())
                .map(|e| (e.mode(), e.oid().to_owned()));
            let Some((mode, id)) = entry else {
                return Ok(None);
            };
            if parts.peek().is_some() {
                if !mode.is_tree() {
                    return Ok(None);
                }
                tree = self.repo.find_tree(id).map_err(|e| object_error(id, e))?;
            } else if mode.is_blob() || mode.is_link() {
                let blob = self.repo.find_blob(id).map_err(|e| object_error(id, e))?;
                return Ok(Some(Blob {
                    bytes: blob.data.clone(),
                    symlink: mode.is_link(),
                }));
            } else {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// Pairs of paths in `commit` that differ only in letter case. A case-insensitive
    /// disk (Windows, default macOS) can hold only one of each pair, so a review on such
    /// a machine must not read them from disk as if both existed.
    pub fn case_collisions(&self, commit: CommitId) -> Result<Vec<(RepoPath, RepoPath)>> {
        let mut recorder = gix::traverse::tree::Recorder::default();
        self.tree(commit)?
            .traverse()
            .breadthfirst(&mut recorder)
            .map_err(|e| object_error(commit, e))?;
        let mut seen: std::collections::HashMap<String, String> = Default::default();
        let mut pairs = Vec::new();
        let mut paths: Vec<String> = recorder
            .records
            .iter()
            .map(|r| String::from_utf8(r.filepath.to_vec()).map_err(|_| GitError::NonUtf8Path))
            .collect::<Result<_>>()?;
        paths.sort();
        for path in paths {
            if let Some(first) = seen.insert(path.to_lowercase(), path.clone()) {
                pairs.push((RepoPath::parse(&first)?, RepoPath::parse(&path)?));
            }
        }
        Ok(pairs)
    }

    /// A file from the working tree, for reviewing uncommitted changes. Refused when the
    /// path, after following links (symlinks, and junctions on Windows), ends up outside
    /// the repository, when it isn't a regular file (a FIFO would block forever), and on
    /// Windows when the name is a device or otherwise unsafe there (SECURITY §14.7).
    pub fn read_worktree_file(&self, path: &RepoPath) -> Result<Vec<u8>> {
        let root = self.workdir.as_ref().ok_or(GitError::NoWorktree)?;
        if cfg!(windows)
            && let Some(problem) = path.windows_problem()
        {
            return Err(GitError::WindowsUnsafe {
                path: path.clone(),
                problem,
            });
        }

        let read_error = |e: std::io::Error| GitError::Read {
            path: path.clone(),
            reason: e.kind().to_string(),
        };
        let joined = path
            .as_str()
            .split('/')
            .fold(root.clone(), |dir, part| dir.join(part));
        let real = joined.canonicalize().map_err(read_error)?;
        if !real.starts_with(root) {
            return Err(GitError::Escape { path: path.clone() });
        }
        // A link that stays inside the working tree can still land in `.git` (its
        // config, hooks, objects). Refuse the git dir and any `.git`-named part.
        let in_git_dir = self
            .git_dir
            .as_ref()
            .is_some_and(|git| real.starts_with(git));
        let relative: Vec<String> = real
            .strip_prefix(root)
            .map(|rest| {
                rest.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        if in_git_dir || RepoPath::parse(&relative.join("/")).is_err() {
            return Err(GitError::GitMetadata { path: path.clone() });
        }
        if !std::fs::metadata(&real).map_err(read_error)?.is_file() {
            return Err(GitError::NotRegularFile { path: path.clone() });
        }
        std::fs::read(&real).map_err(read_error)
    }
}
