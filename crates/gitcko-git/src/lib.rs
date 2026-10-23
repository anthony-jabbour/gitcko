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
    #[error("{path}: goes through a symbolic link or junction, which isn't followed")]
    Link { path: RepoPath },
    #[error("{path}: inside git metadata, which isn't review content")]
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

/// Every gitcko-git call returns [`GitError`], so callers handle one error type.
pub type Result<T, E = GitError> = std::result::Result<T, E>;

/// A commit id (the full hex hash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommitId(gix::ObjectId);

impl std::fmt::Display for CommitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A git object id as stored in a tree entry: a blob's id, or for a submodule the id of
/// a commit in another repository. Ids are content hashes, so naming one in an error
/// reveals nothing about the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(gix::ObjectId);

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// What a tree entry is, from its mode, read the way git reads it (`canon_mode` in git):
/// any regular-file mode is a file, executable when the owner exec bit is set, so the
/// `100664` some old git versions wrote is a plain file. Folders aren't listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A regular file: `100644`, or a legacy mode like `100664`.
    File,
    /// A regular file with the owner exec bit: `100755`, or a legacy mode like `100775`.
    Executable,
    /// Mode `120000`: the blob is the link's target, which is never followed.
    Symlink,
    /// Mode `160000`: a commit in another repository, which isn't here to read.
    Submodule,
    /// A mode that isn't a file, folder, link or gitlink at all. Git never writes one,
    /// but a crafted tree can hold it.
    Other,
}

/// One file-like entry of a commit's tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// The full path as stored, `/`-separated. Raw bytes, not a [`RepoPath`]: a tree can
    /// hold names that aren't valid UTF-8 or aren't safe paths, and discovery has to
    /// record those too rather than stop at them.
    pub path: Vec<u8>,
    pub kind: EntryKind,
    pub id: ObjectId,
}

/// A file's content at a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub bytes: Vec<u8>,
    /// A symbolic link: `bytes` is the link's target, which is never followed.
    pub symlink: bool,
}

/// A repository opened for reading. Nothing here writes to it, so a review can run in
/// the same checkout the user is working in.
pub struct Repo {
    repo: gix::Repository,
    /// The working tree, canonical, when there is one.
    workdir: Option<PathBuf>,
    /// The git dir and common dir, canonical. Either can sit inside the working tree
    /// under any name (`git init --separate-git-dir meta`), so reads compare against
    /// them, not against the name `.git`.
    metadata: Vec<PathBuf>,
}

/// gix's error text can include local paths (a loose object's file, for one), so only
/// the object id goes into ours; the gix error itself is dropped.
fn object_error(id: impl std::fmt::Display, _err: impl std::fmt::Display) -> GitError {
    GitError::Object { id: id.to_string() }
}

impl Repo {
    /// Opens the repository at `path` with gix's isolated options: only the
    /// repository's own `.git/config` is read, never the user's or the system's, so
    /// another machine's git settings can't change what a review sees.
    pub fn open(path: &Path) -> Result<Repo> {
        let repo =
            gix::open_opts(path, gix::open::Options::isolated()).map_err(|_| GitError::Open)?;
        let workdir = repo.workdir().and_then(|dir| dir.canonicalize().ok());
        let metadata = [repo.git_dir(), repo.common_dir()]
            .iter()
            .filter_map(|dir| dir.canonicalize().ok())
            .collect();
        Ok(Repo {
            repo,
            workdir,
            metadata,
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
            // A tree entry that can't be decoded is corruption, not a missing file.
            let mut entry = None;
            for e in tree.iter() {
                let e = e.map_err(|err| object_error(tree.id, err))?;
                if e.filename() == part.as_bytes() {
                    entry = Some((e.mode(), e.oid().to_owned()));
                    break;
                }
            }
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

    /// Every entry of `commit`'s tree except folders, sorted by path bytes, so index
    /// discovery sees the whole commit with no checkout and no disk reads. Submodules are
    /// listed but never entered.
    ///
    /// ponytail: the whole listing is held in memory (about 100 bytes per entry); stream
    /// it if repositories with millions of files ever need indexing.
    pub fn files(&self, commit: CommitId) -> Result<Vec<TreeEntry>> {
        let mut recorder = gix::traverse::tree::Recorder::default();
        self.tree(commit)?
            .traverse()
            .breadthfirst(&mut recorder)
            .map_err(|e| object_error(commit, e))?;
        let mut files: Vec<TreeEntry> = recorder
            .records
            .into_iter()
            .filter(|r| !r.mode.is_tree())
            .map(|r| {
                let kind = if r.mode.is_link() {
                    EntryKind::Symlink
                } else if r.mode.is_executable() {
                    EntryKind::Executable
                } else if r.mode.is_blob() {
                    EntryKind::File
                } else if r.mode.is_commit() {
                    EntryKind::Submodule
                } else {
                    EntryKind::Other
                };
                TreeEntry {
                    path: r.filepath.into(),
                    kind,
                    id: ObjectId(r.oid),
                }
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }

    /// A blob's size from its object header, without inflating the blob, so a huge file
    /// is refused before any of it is read (INDEXING §35.2).
    pub fn blob_size(&self, id: ObjectId) -> Result<u64> {
        let header = self
            .repo
            .find_header(id.0)
            .map_err(|e| object_error(id, e))?;
        if header.kind() != gix::object::Kind::Blob {
            return Err(object_error(id, "not a blob"));
        }
        Ok(header.size())
    }

    /// A blob's bytes by id. Check [`Repo::blob_size`] first: this reads the whole blob.
    pub fn read_blob_id(&self, id: ObjectId) -> Result<Vec<u8>> {
        let blob = self.repo.find_blob(id.0).map_err(|e| object_error(id, e))?;
        Ok(blob.data.clone())
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

    /// A file from the working tree, for reviewing uncommitted changes. Symbolic links
    /// and Windows junctions are never followed, anywhere in the path, so a read can't
    /// leave the repository or reach `.git`. Special files are refused (a FIFO would
    /// block), and on Windows so are names unsafe there, like `CON` (SECURITY §14.7).
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
        read_without_links(root, path, &self.metadata)
    }
}

/// Opens each folder from the repository root with `O_NOFOLLOW`, then the file the same
/// way, and checks the opened handle. Nothing is looked up twice, so another process
/// swapping a part of the path for a link can't redirect the read (no check-then-use
/// gap). Each opened folder is compared with the git metadata folders by device and
/// inode, so no spelling of their names (`META` on a case-insensitive disk) gets in.
#[cfg(unix)]
fn read_without_links(root: &Path, path: &RepoPath, metadata: &[PathBuf]) -> Result<Vec<u8>> {
    use rustix::fs::{FileType, Mode, OFlags, fstat, open, openat, stat};
    use std::io::Read;

    let error = |e: rustix::io::Errno| match e {
        // A link where a folder or the file was expected.
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => {
            GitError::Link { path: path.clone() }
        }
        e => GitError::Read {
            path: path.clone(),
            reason: std::io::Error::from(e).kind().to_string(),
        },
    };
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let parts: Vec<&str> = path.as_str().split('/').collect();
    let (file_name, folders) = parts
        .split_last()
        .expect("a RepoPath has at least one part");
    let metadata: Vec<_> = metadata
        .iter()
        .filter_map(|dir| stat(dir).ok())
        .map(|st| (st.st_dev, st.st_ino))
        .collect();
    let mut dir = open(root, dir_flags, Mode::empty()).map_err(error)?;
    for folder in folders {
        dir = openat(&dir, *folder, dir_flags | OFlags::NOFOLLOW, Mode::empty()).map_err(error)?;
        let st = fstat(&dir).map_err(error)?;
        if metadata.contains(&(st.st_dev, st.st_ino)) {
            return Err(GitError::GitMetadata { path: path.clone() });
        }
    }
    let file_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = openat(&dir, *file_name, file_flags, Mode::empty()).map_err(error)?;
    if FileType::from_raw_mode(fstat(&fd).map_err(error)?.st_mode) != FileType::RegularFile {
        return Err(GitError::NotRegularFile { path: path.clone() });
    }
    let mut bytes = Vec::new();
    std::fs::File::from(fd)
        .read_to_end(&mut bytes)
        .map_err(|e| GitError::Read {
            path: path.clone(),
            reason: e.kind().to_string(),
        })?;
    Ok(bytes)
}

/// std has no `openat` on Windows, so each part is checked for a link or junction, the
/// file is read, and the parts are checked again. A swap and swap-back between those
/// checks isn't caught; Unix has no such window.
#[cfg(not(unix))]
fn read_without_links(root: &Path, path: &RepoPath, metadata: &[PathBuf]) -> Result<Vec<u8>> {
    let read_error = |e: std::io::Error| GitError::Read {
        path: path.clone(),
        reason: e.kind().to_string(),
    };
    let check = || -> Result<PathBuf> {
        let mut at = root.to_path_buf();
        for part in path.as_str().split('/') {
            at.push(part);
            if std::fs::symlink_metadata(&at)
                .map_err(read_error)?
                .file_type()
                .is_symlink()
            {
                return Err(GitError::Link { path: path.clone() });
            }
        }
        Ok(at)
    };
    let full = check()?;
    // Canonical paths fold case and short names on Windows, so any spelling matches.
    let canonical = full.canonicalize().map_err(read_error)?;
    if metadata.iter().any(|dir| canonical.starts_with(dir)) {
        return Err(GitError::GitMetadata { path: path.clone() });
    }
    if !std::fs::symlink_metadata(&full)
        .map_err(read_error)?
        .is_file()
    {
        return Err(GitError::NotRegularFile { path: path.clone() });
    }
    let bytes = std::fs::read(&full).map_err(read_error)?;
    check()?;
    Ok(bytes)
}
