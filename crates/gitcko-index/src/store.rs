// SPDX-License-Identifier: AGPL-3.0-or-later

//! The local index store (INDEXING §24–26, §31): validated revisions on disk, published
//! atomically, checked on every load, purged and evicted without touching one in use.
//!
//! ```text
//! <root>/repos/<repo-id>/last_used
//! <root>/repos/<repo-id>/index/<versions-label>/{active, publish.lock, staging/,
//!     revisions/<rev-id>/{lock, revision.json, manifest.json, parsed.json,
//!                         chunks.json, coverage.json, lexical/}}
//! ```
//!
//! The versions label names the folder, so a schema or parser change starts a fresh one
//! that never reads the old (the lazy rebuild), and the old one is purged as stale. Files
//! are named by the store, never after repository paths. Folders are `0700`, files `0600`.
//! The graph isn't stored: it's re-resolved from the stored extractions, which is cheap and
//! deterministic, and the whole revision is validated against the commit again on load.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use gitcko_domain::{ContentHash, RepoPath};
use gitcko_git::Repo;
use gitcko_search::LexicalIndex;

use crate::build::{lexical_docs, resolve_graph};
use crate::{Chunk, Manifest, ParsedFile, Revision, RevisionCoverage, Versions, validate};

/// Changes whenever the files a stored revision is made of change.
const STORE_FORMAT: u32 = 1;

const DATA_FILES: [&str; 4] = [
    "manifest.json",
    "parsed.json",
    "chunks.json",
    "coverage.json",
];

/// A local index store under one root folder.
pub struct Store {
    root: PathBuf,
}

/// The steps of a publish, in order. A publish stopped before any of them leaves `active`
/// where it was; one stopped before [`PublishStep::Activate`] may leave a complete, unused
/// revision, which purge removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishStep {
    /// Writing the revision's files into a staging folder.
    Write,
    /// Flushing them to disk.
    Sync,
    /// Moving the staging folder to `revisions/<rev-id>`.
    Rename,
    /// Pointing `active` at it.
    Activate,
}

impl PublishStep {
    /// Every step, in order.
    pub const ALL: [PublishStep; 4] = [
        PublishStep::Write,
        PublishStep::Sync,
        PublishStep::Rename,
        PublishStep::Activate,
    ];
}

/// Why a store operation failed. Never names a path or a value from the repository.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("a repository id must be 1 to 64 characters of a-z, 0-9, _ and -")]
    InvalidRepoId,
    #[error("no such revision in the store")]
    NotFound,
    /// The revision on disk doesn't match what was published; rebuild it.
    #[error("the stored revision is corrupt ({check})")]
    Corrupt { check: &'static str },
    #[error("the revision is being read, so it can't be removed")]
    InUse,
    #[error("publishing was cancelled")]
    Cancelled,
    #[error("the store couldn't {op}")]
    Io { op: &'static str },
}

/// A revision read from the store. Holds a shared lock on it, so purge leaves it alone
/// until this is dropped.
pub struct StoredRevision {
    revision: Revision,
    _lock: File,
}

impl Deref for StoredRevision {
    type Target = Revision;

    fn deref(&self) -> &Revision {
        &self.revision
    }
}

/// `revision.json`: what the revision is, and the SHA-256 of every file it's made of.
#[derive(serde::Serialize, serde::Deserialize)]
struct Header {
    format: u32,
    commit: String,
    versions: Versions,
    files: BTreeMap<String, ContentHash>,
}

fn io(op: &'static str) -> impl Fn(std::io::Error) -> StoreError {
    move |_| StoreError::Io { op }
}

impl Store {
    /// A store rooted at `root`, created on first publish.
    pub fn new(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    /// Writes `revision` and makes it the repository's active one, returning its id. A
    /// revision already stored intact isn't written again. `keep_going` is asked before
    /// each step; answering `false` stops cleanly (cancellation).
    pub fn publish(
        &self,
        repo_id: &str,
        revision: &Revision,
        keep_going: &mut dyn FnMut(PublishStep) -> bool,
    ) -> Result<String, StoreError> {
        let dir = self.index_dir(repo_id)?;
        let staging = dir.join("staging");
        let revisions = dir.join("revisions");
        make_dir(&staging).map_err(io("create the store"))?;
        make_dir(&revisions).map_err(io("create the store"))?;
        // One publisher per index folder; anything left in staging is from a crash.
        let lock = create_file(&dir.join("publish.lock")).map_err(io("lock the store"))?;
        lock.lock().map_err(io("lock the store"))?;
        for e in fs::read_dir(&staging).map_err(io("read the store"))? {
            let _ = fs::remove_dir_all(e.map_err(io("read the store"))?.path());
        }

        let id = revision_id(&revision.commit, &revision.versions);
        let target = revisions.join(&id);
        if target.exists() && read_header(&target, &id).is_err() {
            remove_unlocked(&target)?;
        }
        if !target.exists() {
            let temp = tempfile::Builder::new()
                .tempdir_in(&staging)
                .map_err(io("create the store"))?;
            if !keep_going(PublishStep::Write) {
                return Err(StoreError::Cancelled);
            }
            restrict_dir(temp.path()).map_err(io("create the store"))?;
            write_revision(temp.path(), revision)?;
            if !keep_going(PublishStep::Sync) {
                return Err(StoreError::Cancelled);
            }
            sync_tree(temp.path()).map_err(io("write the store"))?;
            if !keep_going(PublishStep::Rename) {
                return Err(StoreError::Cancelled);
            }
            let staged = temp.keep();
            fs::rename(&staged, &target).map_err(io("publish a revision"))?;
            sync_dir(&revisions).map_err(io("publish a revision"))?;
        }
        if !keep_going(PublishStep::Activate) {
            return Err(StoreError::Cancelled);
        }
        // Replace `active` by renaming a complete file over it: never half-written.
        let temp = tempfile::Builder::new()
            .tempfile_in(&staging)
            .map_err(io("publish a revision"))?;
        temp.as_file()
            .write_all(id.as_bytes())
            .and_then(|()| temp.as_file().sync_all())
            .map_err(io("publish a revision"))?;
        temp.persist(dir.join("active"))
            .map_err(|_| StoreError::Io {
                op: "publish a revision",
            })?;
        sync_dir(&dir).map_err(io("publish a revision"))?;
        self.touch(repo_id);
        Ok(id)
    }

    /// The repository's active revision id, if it has one.
    pub fn active(&self, repo_id: &str) -> Result<Option<String>, StoreError> {
        let dir = self.index_dir(repo_id)?;
        match fs::read_to_string(dir.join("active")) {
            Ok(id) if is_revision_id(&id) => Ok(Some(id)),
            Ok(_) => Err(StoreError::Corrupt { check: "active" }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(StoreError::Io {
                op: "read the store",
            }),
        }
    }

    /// Loads revision `id`: checks every file against its recorded hash, re-resolves the
    /// graph and validates the whole revision against the commit in `repo` (§26). Any
    /// mismatch is [`StoreError::Corrupt`], never a panic; the next publish replaces it.
    pub fn open(&self, repo: &Repo, repo_id: &str, id: &str) -> Result<StoredRevision, StoreError> {
        let dir = self.index_dir(repo_id)?;
        if !is_revision_id(id) {
            return Err(StoreError::NotFound);
        }
        let rdir = dir.join("revisions").join(id);
        if !rdir.is_dir() {
            return Err(StoreError::NotFound);
        }
        let loaded = load(repo, &rdir, id);
        match &loaded {
            Ok(_) => self.touch(repo_id),
            // Removed, a corrupt revision is rebuilt by the next publish instead of being
            // found again. Best effort: another reader may still hold it.
            Err(StoreError::Corrupt { .. }) => {
                let _ = remove_unlocked(&rdir);
            }
            Err(_) => {}
        }
        loaded
    }

    /// Removes the repository's revisions that aren't active, and index folders from
    /// other versions, skipping any revision being read. Returns how many were removed.
    pub fn purge_stale(&self, repo_id: &str) -> Result<usize, StoreError> {
        let current = self.index_dir(repo_id)?;
        let Some(base) = current.parent() else {
            return Ok(0);
        };
        let active = self.active(repo_id).ok().flatten();
        let mut removed = 0;
        for e in read_dir_or_empty(base)? {
            if e != current {
                if remove_unlocked(&e).is_ok() {
                    removed += 1;
                }
                continue;
            }
            for r in read_dir_or_empty(&e.join("revisions"))? {
                let name = r.file_name().and_then(|n| n.to_str()).map(str::to_owned);
                if name != active && remove_unlocked(&r).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Removes everything stored for one repository, unless a revision is being read.
    pub fn purge_repo(&self, repo_id: &str) -> Result<(), StoreError> {
        check_repo_id(repo_id)?;
        let dir = self.root.join("repos").join(repo_id);
        if dir.exists() {
            remove_unlocked(&dir)?;
        }
        Ok(())
    }

    /// Removes every repository's index, skipping those being read
    /// ([`StoreError::InUse`] once the rest are gone).
    pub fn purge_all(&self) -> Result<(), StoreError> {
        let mut result = Ok(());
        for (id, _) in self.sizes()? {
            if let Err(e) = self.purge_repo(&id) {
                result = Err(e);
            }
        }
        result
    }

    /// Bytes stored per repository, sorted by repository id.
    pub fn sizes(&self) -> Result<Vec<(String, u64)>, StoreError> {
        let mut out = Vec::new();
        for dir in read_dir_or_empty(&self.root.join("repos"))? {
            let Some(id) = dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if check_repo_id(id).is_ok() {
                out.push((id.to_owned(), tree_size(&dir)));
            }
        }
        out.sort();
        Ok(out)
    }

    /// Evicts the least recently used repositories until the store fits in `cap` bytes,
    /// never `keep` (the one being worked on) or one being read. Returns those evicted.
    pub fn evict_to(&self, cap: u64, keep: &str) -> Result<Vec<String>, StoreError> {
        let sizes = self.sizes()?;
        let mut total: u64 = sizes.iter().map(|(_, s)| s).sum();
        let mut by_age: Vec<(u128, String, u64)> = sizes
            .into_iter()
            .filter(|(id, _)| id != keep)
            .map(|(id, size)| (self.last_used(&id), id, size))
            .collect();
        by_age.sort();
        let mut evicted = Vec::new();
        for (_, id, size) in by_age {
            if total <= cap {
                break;
            }
            if self.purge_repo(&id).is_ok() {
                total -= size;
                evicted.push(id);
            }
        }
        Ok(evicted)
    }

    fn index_dir(&self, repo_id: &str) -> Result<PathBuf, StoreError> {
        check_repo_id(repo_id)?;
        Ok(self
            .root
            .join("repos")
            .join(repo_id)
            .join("index")
            .join(Versions::current().label()))
    }

    /// Records use for LRU eviction. Best effort: a failure only makes eviction less fair.
    fn touch(&self, repo_id: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = self.root.join("repos").join(repo_id).join("last_used");
        let _ = create_file(&path).and_then(|mut f| f.write_all(now.to_string().as_bytes()));
    }

    fn last_used(&self, repo_id: &str) -> u128 {
        fs::read_to_string(self.root.join("repos").join(repo_id).join("last_used"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }
}

/// Reads, checks and validates the revision in `rdir`, holding its shared lock.
fn load(repo: &Repo, rdir: &Path, id: &str) -> Result<StoredRevision, StoreError> {
    let lock = File::open(rdir.join("lock")).map_err(|_| StoreError::Corrupt { check: "lock" })?;
    lock.lock_shared().map_err(io("lock a revision"))?;
    let header = read_header(rdir, id)?;
    let manifest: Manifest = read_json(rdir, "manifest.json")?;
    let parsed: Vec<ParsedFile> = read_json(rdir, "parsed.json")?;
    let chunks: Vec<Chunk> = read_json(rdir, "chunks.json")?;
    let mut coverage: RevisionCoverage = read_json(rdir, "coverage.json")?;
    if parsed.iter().any(|p| p.entry >= manifest.entries.len()) {
        return Err(StoreError::Corrupt { check: "parsed" });
    }
    let objects: BTreeMap<RepoPath, gitcko_git::ObjectId> = manifest
        .entries
        .iter()
        .filter_map(|e| Some((e.repo_path.clone()?, e.object)))
        .collect();
    let source = |p: &RepoPath| {
        let bytes = repo.read_blob_id(*objects.get(p)?).ok()?;
        String::from_utf8(bytes).ok()
    };
    let graph = resolve_graph(&manifest, &parsed, source);
    coverage.graph = graph.coverage.clone();
    let lexical = LexicalIndex::open(&rdir.join("lexical"))
        .map_err(|_| StoreError::Corrupt { check: "lexical" })?;
    let revision = Revision {
        commit: header.commit,
        manifest,
        parsed,
        graph,
        chunks,
        lexical,
        coverage,
        versions: header.versions,
    };
    validate(&revision, source).map_err(|_| StoreError::Corrupt { check: "validate" })?;
    Ok(StoredRevision {
        revision,
        _lock: lock,
    })
}

/// A revision's id: the commit and every version it was built with, so an incremental
/// and a clean build of one commit share it, and republishing it is a no-op.
fn revision_id(commit: &str, versions: &Versions) -> String {
    ContentHash::of(format!("{commit}\n{}", versions.label()).as_bytes()).to_string()
}

fn is_revision_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Repository ids name a folder, so they're held to a set no OS treats specially.
fn check_repo_id(id: &str) -> Result<(), StoreError> {
    let ok = (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(StoreError::InvalidRepoId)
    }
}

fn write_revision(dir: &Path, revision: &Revision) -> Result<(), StoreError> {
    let write = |name: &str, value: &dyn erased::Json| -> Result<(), StoreError> {
        let bytes = value.to_json().map_err(|_| StoreError::Io {
            op: "write the store",
        })?;
        create_file(&dir.join(name))
            .and_then(|mut f| f.write_all(&bytes))
            .map_err(io("write the store"))
    };
    write("manifest.json", &revision.manifest)?;
    write("parsed.json", &revision.parsed)?;
    write("chunks.json", &revision.chunks)?;
    write("coverage.json", &revision.coverage)?;
    let docs = lexical_docs(&revision.manifest, &revision.graph, &revision.chunks).ok_or(
        StoreError::Io {
            op: "write the store",
        },
    )?;
    let lexical = dir.join("lexical");
    make_dir(&lexical).map_err(io("write the store"))?;
    LexicalIndex::build(Some(&lexical), docs).map_err(|_| StoreError::Io {
        op: "write the store",
    })?;
    restrict(&lexical).map_err(io("write the store"))?;
    let mut files = BTreeMap::new();
    for name in DATA_FILES {
        files.insert(name.to_owned(), hash_file(&dir.join(name))?);
    }
    for (name, path) in walk_files(&lexical, "lexical").map_err(io("write the store"))? {
        files.insert(name, hash_file(&path)?);
    }
    let header = Header {
        format: STORE_FORMAT,
        commit: revision.commit.clone(),
        versions: revision.versions.clone(),
        files,
    };
    write("revision.json", &header)?;
    create_file(&dir.join("lock")).map_err(io("write the store"))?;
    Ok(())
}

/// Reads `revision.json` and checks that every file it lists is there and unchanged.
fn read_header(dir: &Path, id: &str) -> Result<Header, StoreError> {
    let header: Header = read_json(dir, "revision.json")?;
    // The id hashes the versions, and the folder is named for the current ones, so a
    // matching id is also a version check.
    if header.format != STORE_FORMAT || revision_id(&header.commit, &header.versions) != id {
        return Err(StoreError::Corrupt { check: "header" });
    }
    if DATA_FILES.iter().any(|f| !header.files.contains_key(*f)) {
        return Err(StoreError::Corrupt { check: "files" });
    }
    for (name, hash) in &header.files {
        // Names come from the file, so only plain relative names are followed.
        let safe = name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains('\\'));
        if !safe || hash_file(&dir.join(name)).ok() != Some(*hash) {
            return Err(StoreError::Corrupt { check: "checksum" });
        }
    }
    Ok(header)
}

fn read_json<T: serde::de::DeserializeOwned>(dir: &Path, name: &str) -> Result<T, StoreError> {
    let bytes = fs::read(dir.join(name)).map_err(|_| StoreError::Corrupt { check: "missing" })?;
    serde_json::from_slice(&bytes).map_err(|_| StoreError::Corrupt { check: "decode" })
}

fn hash_file(path: &Path) -> Result<ContentHash, StoreError> {
    fs::read(path)
        .map(|b| ContentHash::of(&b))
        .map_err(|_| StoreError::Corrupt { check: "missing" })
}

/// Removes a revision folder, or a folder of them, unless one is being read: each
/// revision's lock must be free. ponytail: a reader could take a lock between the check
/// and the removal; it then fails to load and rebuilds. A per-revision rename-then-delete
/// closes that window if it ever matters.
fn remove_unlocked(dir: &Path) -> Result<(), StoreError> {
    let mut locks = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let lock = d.join("lock");
        if lock.is_file() {
            let f = File::open(&lock).map_err(io("lock a revision"))?;
            if f.try_lock().is_err() {
                return Err(StoreError::InUse);
            }
            locks.push(f);
            continue;
        }
        stack.extend(read_dir_or_empty(&d)?.into_iter().filter(|p| p.is_dir()));
    }
    drop(locks);
    fs::remove_dir_all(dir).map_err(io("remove a revision"))
}

fn read_dir_or_empty(dir: &Path) -> Result<Vec<PathBuf>, StoreError> {
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| e.map(|e| e.path()).map_err(io("read the store")))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err(StoreError::Io {
            op: "read the store",
        }),
    }
}

/// Every file under `dir`, named `prefix/relative/path`, sorted.
fn walk_files(dir: &Path, prefix: &str) -> std::io::Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        let name = format!("{prefix}/{}", e.file_name().to_string_lossy());
        if e.file_type()?.is_dir() {
            out.extend(walk_files(&e.path(), &name)?);
        } else {
            out.push((name, e.path()));
        }
    }
    out.sort();
    Ok(out)
}

fn tree_size(dir: &Path) -> u64 {
    walk_files(dir, "")
        .unwrap_or_default()
        .iter()
        .filter_map(|(_, p)| p.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn make_dir(path: &Path) -> std::io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(path)
}

fn create_file(path: &Path) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
}

/// `0700`, for a folder made by something else (`tempfile` makes them `0755`).
fn restrict_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let _ = dir;
    Ok(())
}

/// Tantivy creates its own files with the default mode; narrow them to the store's.
fn restrict(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for e in fs::read_dir(dir)? {
            let e = e?;
            if e.file_type()?.is_dir() {
                fs::set_permissions(e.path(), fs::Permissions::from_mode(0o700))?;
                restrict(&e.path())?;
            } else {
                fs::set_permissions(e.path(), fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    let _ = dir;
    Ok(())
}

/// Flushes every file under `dir`, then the folders, so a rename publishes only data
/// that is on disk.
fn sync_tree(dir: &Path) -> std::io::Result<()> {
    for (_, path) in walk_files(dir, "")? {
        File::open(path)?.sync_all()?;
    }
    sync_dir(dir)
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    // Windows can't open a folder as a file; its renames are flushed by the file system.
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    let _ = dir;
    Ok(())
}

/// Serializing values of different types through one closure.
mod erased {
    pub trait Json {
        fn to_json(&self) -> serde_json::Result<Vec<u8>>;
    }

    impl<T: serde::Serialize> Json for T {
        fn to_json(&self) -> serde_json::Result<Vec<u8>> {
            serde_json::to_vec(self)
        }
    }
}
