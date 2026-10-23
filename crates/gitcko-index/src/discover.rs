// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::HashMap;

use gitcko_config::{FolderConfig, IgnoreRules, RepoFiles, ignore_rules};
use gitcko_domain::{ContentHash, Coverage, Reason, RepoPath};
use gitcko_git::{CommitId, EntryKind, GitError, ObjectId, Repo, TreeEntry};
use gitcko_syntax::{FileRole, Language, classify};

use crate::IndexLimits;

/// What the index does with a path (INDEXING §8). Every path in the commit gets exactly
/// one, so nothing is silently skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Parsed into symbols and edges, and searched as text.
    Parse,
    /// Searched as text only: no parser for its language, or too big to parse.
    LexicalOnly,
    /// Only its path, size and kind are recorded: binary, a symlink, a submodule, an LFS
    /// pointer, or not UTF-8.
    MetadataOnly,
    /// Refused by policy: a name that isn't a safe repository path.
    ExcludedPolicy,
    /// Over a size or count limit.
    ExcludedResourceLimit,
    /// Couldn't be read as a path or file at all (a non-UTF-8 name, an unknown mode).
    Unavailable,
}

/// One path of the commit and what the index will do with it (INDEXING §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// The path as stored in the tree, for display. A name that isn't UTF-8 has its bytes
    /// escaped (`bad\xff.rs`), and a backslash in any name is doubled, so two distinct
    /// names never display the same.
    pub path: String,
    /// The validated path, when the name is a safe repository path.
    pub repo_path: Option<RepoPath>,
    pub kind: EntryKind,
    /// The git object, to read the content again later.
    pub object: ObjectId,
    /// File size, for files whose size was looked up.
    pub size: Option<u64>,
    /// SHA-256 of the content, for files that were read.
    pub content_hash: Option<ContentHash>,
    pub language: Option<Language>,
    pub role: FileRole,
    pub binary: bool,
    pub generated: bool,
    pub vendor: bool,
    /// Matches the config's `ignore` list. The file stays indexed: the list says what a
    /// review skips, and dropping vendored or generated code from the index would hide
    /// real dependency paths (INDEXING §14.7). Retrieval ranks these lower instead.
    pub ignored_by_config: bool,
    /// Another path in the commit differs from this one only in letter case, so a
    /// case-insensitive disk could hold only one of them. Both stay indexed: the index
    /// reads git objects, not the disk.
    pub case_collision: bool,
    pub disposition: Disposition,
    /// Why the disposition isn't the obvious one (`binary`, `file_too_large`, …).
    pub reason: Option<Reason>,
}

/// Every path of one commit, in path-byte order, with its disposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub entries: Vec<ManifestEntry>,
    /// `partial` when a repository-wide limit cut files off.
    pub coverage: Coverage,
    /// The repository's config couldn't be used (the root file is invalid, or the
    /// `.gitcko/` files are over their budget), so `ignore` used the built-in defaults.
    pub config_invalid: bool,
}

/// What went wrong while indexing. Messages never include a path or file content from
/// the repository, which are untrusted and can reach logs; git objects are named by id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IndexError {
    #[error("can't read git object {id}: missing or corrupt")]
    Object { id: String },
    #[error("can't read the repository")]
    Repository,
}

/// Keeps only what's safe to show: an object id. Every other git error (some name a
/// repository path) becomes the plain [`IndexError::Repository`].
impl From<GitError> for IndexError {
    fn from(err: GitError) -> IndexError {
        match err {
            GitError::Object { id } => IndexError::Object { id },
            _ => IndexError::Repository,
        }
    }
}

/// Lists every path of `commit` and decides what the index does with each (INDEXING
/// §7–9). Reads blobs only up to `limits`, checking each size before reading.
pub fn discover(
    repo: &Repo,
    commit: CommitId,
    limits: &IndexLimits,
) -> Result<Manifest, IndexError> {
    let files = repo.files(commit)?;
    let (ignore, config_invalid) = ignore_list(repo, &files, limits)?;
    let mut entries = Vec::with_capacity(files.len());
    // Repository-wide limits cut a prefix of the sorted paths: once one is reached,
    // every later file is left out too, so the result doesn't depend on file order.
    let mut cut: Option<&'static str> = None;
    let (mut read_files, mut read_bytes, mut omitted) = (0usize, 0u64, 0u64);
    let mut unreadable = 0u64;
    for file in files {
        let mut entry = ManifestEntry {
            path: display_path(&file.path),
            repo_path: None,
            kind: file.kind,
            object: file.id,
            size: None,
            content_hash: None,
            language: None,
            role: FileRole::Unknown,
            binary: false,
            generated: false,
            vendor: false,
            ignored_by_config: false,
            case_collision: false,
            disposition: Disposition::Unavailable,
            reason: None,
        };
        let Ok(text) = std::str::from_utf8(&file.path) else {
            entry.reason = Some(reason("non_utf8_path"));
            entries.push(entry);
            continue;
        };
        let Ok(path) = RepoPath::parse(text) else {
            // `..`, `.git`, `CON` and the like: never a file the index will read.
            entry.disposition = Disposition::ExcludedPolicy;
            entry.reason = Some(reason("unsafe_path"));
            entries.push(entry);
            continue;
        };
        let by_name = classify(&path, b"");
        (entry.language, entry.role) = (by_name.language, by_name.role);
        (entry.generated, entry.vendor) = (by_name.generated, by_name.vendor);
        entry.ignored_by_config = ignore.is_ignored(&path);
        entry.repo_path = Some(path.clone());
        let metadata_only = match file.kind {
            EntryKind::Symlink => Some("symlink"),
            EntryKind::Submodule => Some("submodule"),
            EntryKind::Other => {
                entry.reason = Some(reason("unknown_mode"));
                entries.push(entry);
                continue;
            }
            EntryKind::File | EntryKind::Executable => None,
        };
        if let Some(why) = metadata_only {
            entry.disposition = Disposition::MetadataOnly;
            entry.reason = Some(reason(why));
            entries.push(entry);
            continue;
        }

        // A missing or corrupt object costs that one file, not the whole manifest
        // (INDEXING §36).
        let Ok(size) = repo.blob_size(file.id) else {
            unreadable += 1;
            entry.reason = Some(reason("unreadable"));
            entries.push(entry);
            continue;
        };
        entry.size = Some(size);
        if cut.is_none() && read_files >= limits.max_files {
            cut = Some("repo_file_limit");
        }
        if cut.is_none()
            && size <= limits.max_lexical_bytes
            && read_bytes + size > limits.max_repo_bytes
        {
            cut = Some("repo_byte_limit");
        }
        if let Some(why) = cut {
            omitted += 1;
            entry.disposition = Disposition::ExcludedResourceLimit;
            entry.reason = Some(reason(why));
            entries.push(entry);
            continue;
        }
        if size > limits.max_lexical_bytes {
            entry.disposition = Disposition::ExcludedResourceLimit;
            entry.reason = Some(reason("file_too_large"));
            entries.push(entry);
            continue;
        }
        read_files += 1;
        read_bytes += size;
        let Ok(bytes) = repo.read_blob_id(file.id) else {
            unreadable += 1;
            entry.reason = Some(reason("unreadable"));
            entries.push(entry);
            continue;
        };
        entry.content_hash = Some(ContentHash::of(&bytes));
        let class = classify(&path, &bytes);
        (entry.role, entry.generated) = (class.role, class.generated);
        entry.binary = class.binary;
        let (disposition, why) = if class.lfs_pointer {
            (Disposition::MetadataOnly, Some("lfs_pointer"))
        } else if class.binary {
            (Disposition::MetadataOnly, Some("binary"))
        } else if std::str::from_utf8(&bytes).is_err() {
            (Disposition::MetadataOnly, Some("not_utf8"))
        } else if !class.language.is_some_and(Language::has_adapter) {
            (Disposition::LexicalOnly, None)
        } else if size > limits.max_parse_bytes {
            (Disposition::LexicalOnly, Some("file_too_large_to_parse"))
        } else {
            (Disposition::Parse, None)
        };
        entry.disposition = disposition;
        entry.reason = why.map(reason);
        entries.push(entry);
    }
    mark_case_collisions(&mut entries);
    // `omitted` counts every file left out, whichever the reason; a limit is named first
    // because it's the one the user can raise.
    let coverage = match (cut, unreadable) {
        (Some(why), _) => Coverage::Partial {
            reason: reason(why),
            omitted: Some(omitted + unreadable),
        },
        (None, 1..) => Coverage::Partial {
            reason: reason("unreadable_files"),
            omitted: Some(unreadable),
        },
        (None, 0) => Coverage::Complete {},
    };
    Ok(Manifest {
        entries,
        coverage,
        config_invalid,
    })
}

/// Reason codes here are literals, so a bad one is a bug caught by the tests.
fn reason(code: &str) -> Reason {
    Reason::new(code).expect("reason codes in this file are valid")
}

/// The commit's `ignore` lists. A root config that can't be read (unreadable object, not
/// UTF-8, invalid TOML) doesn't stop indexing: the default list applies, child configs
/// are dropped with it, and the manifest says so. A child config that isn't UTF-8 is
/// skipped, so its folder inherits its parent's list, as for any broken nested config
/// (CONFIGURATION §11).
fn ignore_list(
    repo: &Repo,
    files: &[TreeEntry],
    limits: &IndexLimits,
) -> Result<(IgnoreRules, bool), IndexError> {
    let mut folders: HashMap<Option<RepoPath>, FolderConfig> = HashMap::new();
    let mut legacy_root_file = false;
    let (mut config_files, mut config_bytes) = (0usize, 0u64);
    let defaults = || ignore_rules(&RepoFiles::default()).expect("the built-in defaults are valid");
    for file in files {
        if !matches!(file.kind, EntryKind::File | EntryKind::Executable) {
            continue;
        }
        let Some(path) = std::str::from_utf8(&file.path)
            .ok()
            .and_then(|p| RepoPath::parse(p).ok())
        else {
            continue;
        };
        let text = path.as_str();
        legacy_root_file |= text == ".gitcko.toml";
        let (folder, is_config) = if let Some(folder) = text.strip_suffix(".gitcko/config.toml") {
            (folder, true)
        } else if let Some(folder) = text.strip_suffix(".gitcko/rules.md") {
            (folder, false)
        } else {
            continue;
        };
        // `x.gitcko/config.toml` isn't a `.gitcko/` folder.
        if !(folder.is_empty() || folder.ends_with('/')) {
            continue;
        }
        // Checked before reading, so a repository full of `.gitcko/` folders can't make
        // discovery read more than the budget (TM-38).
        config_files += 1;
        let Ok(size) = repo.blob_size(file.id) else {
            return Ok((defaults(), true));
        };
        config_bytes += size;
        if config_files > limits.max_config_files || config_bytes > limits.max_config_bytes {
            return Ok((defaults(), true));
        }
        let folder = folder
            .strip_suffix('/')
            .map(|f| RepoPath::parse(f).expect("a prefix of a valid path is valid"));
        let Ok(bytes) = repo.read_blob_id(file.id) else {
            return Ok((defaults(), true));
        };
        let Ok(content) = String::from_utf8(bytes) else {
            if folder.is_none() {
                return Ok((defaults(), true));
            }
            continue;
        };
        let entry = folders.entry(folder.clone()).or_insert(FolderConfig {
            folder,
            config_toml: None,
            rules_md: None,
        });
        if is_config {
            entry.config_toml = Some(content);
        } else {
            entry.rules_md = Some(content);
        }
    }
    let repo_files = RepoFiles {
        folders: folders.into_values().collect(),
        legacy_root_file,
    };
    Ok(match ignore_rules(&repo_files) {
        Ok(rules) => (rules, false),
        Err(_) => (defaults(), true),
    })
}

/// The display form of a tree path (see [`ManifestEntry::path`]).
fn display_path(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.replace('\\', "\\\\"),
        // `escape_ascii` escapes backslashes too, so the two forms can't overlap.
        Err(_) => bytes.escape_ascii().to_string(),
    }
}

/// Flags every path that differs from another only in letter case.
fn mark_case_collisions(entries: &mut [ManifestEntry]) {
    let mut by_lowercase: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Some(path) = &entry.repo_path {
            by_lowercase
                .entry(path.as_str().to_lowercase())
                .or_default()
                .push(i);
        }
    }
    for group in by_lowercase.values().filter(|g| g.len() > 1) {
        for &i in group {
            entries[i].case_collision = true;
        }
    }
}
