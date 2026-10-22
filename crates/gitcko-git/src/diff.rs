// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tree-to-tree changes as a [`ChangeSet`], matching what `git diff -M` prints.

use gitcko_diff::{Change, ChangeSet, FileDiff, Hunk, Line, LineKind, Mode};
use gitcko_domain::RepoPath;
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{Algorithm, InternedInput, UnifiedDiff, diff_with_slider_heuristics};
use gix::object::tree::diff::ChangeDetached;
use gix::objs::tree::EntryMode;

use crate::{GitError, Result, object_error};

pub(crate) fn trees(
    repo: &gix::Repository,
    old: gix::Tree<'_>,
    new: gix::Tree<'_>,
) -> Result<ChangeSet> {
    let mut options = gix::diff::Options::default();
    // git's defaults: renames on (diff.renames, 50% similarity), copies off.
    options
        .track_path()
        .track_rewrites(Some(Default::default()));
    let changes = repo
        .diff_tree_to_tree(&old, &new, options)
        .map_err(|e| object_error(format!("{}..{}", old.id, new.id), e))?;
    let mut files = Vec::new();
    for change in changes {
        if let Some(file) = file_diff(repo, change)? {
            files.push(file);
        }
    }
    // git lists files by path.
    files.sort_by(|a, b| sort_key(a).cmp(sort_key(b)));
    Ok(ChangeSet { files })
}

fn sort_key(file: &FileDiff) -> &str {
    file.new_path
        .as_ref()
        .or(file.old_path.as_ref())
        .map_or("", |p| p.as_str())
}

/// One side of a change: its mode and blob.
type Side = (EntryMode, gix::ObjectId);

fn file_diff(repo: &gix::Repository, change: ChangeDetached) -> Result<Option<FileDiff>> {
    let (old, new, old_path, new_path, change) = match change {
        ChangeDetached::Addition {
            entry_mode,
            id,
            location,
            ..
        } => (
            None,
            Some((entry_mode, id)),
            None,
            Some(location),
            Change::Added,
        ),
        ChangeDetached::Deletion {
            entry_mode,
            id,
            location,
            ..
        } => (
            Some((entry_mode, id)),
            None,
            Some(location),
            None,
            Change::Deleted,
        ),
        ChangeDetached::Modification {
            previous_entry_mode,
            previous_id,
            entry_mode,
            id,
            location,
        } => (
            Some((previous_entry_mode, previous_id)),
            Some((entry_mode, id)),
            Some(location.clone()),
            Some(location),
            Change::Modified,
        ),
        ChangeDetached::Rewrite {
            source_location,
            source_entry_mode,
            source_id,
            diff,
            entry_mode,
            id,
            copy,
            location,
            ..
        } => {
            // gix scores similarity by lines; git by bytes. Same 0–100 scale.
            let similarity = if source_id == id {
                100
            } else {
                diff.map_or(0, |d| (d.similarity * 100.0).round() as u8)
            };
            let change = if copy {
                Change::Copied { similarity }
            } else {
                Change::Renamed { similarity }
            };
            (
                Some((source_entry_mode, source_id)),
                Some((entry_mode, id)),
                Some(source_location),
                Some(location),
                change,
            )
        }
    };
    // Folders appear as their own entries; their files are listed separately.
    if old.is_some_and(|(m, _): Side| m.is_tree()) || new.is_some_and(|(m, _): Side| m.is_tree()) {
        return Ok(None);
    }
    let path = |p: Option<gix::bstr::BString>| -> Result<Option<RepoPath>> {
        p.map(|p| {
            let p = String::from_utf8(p.into()).map_err(|_| GitError::NonUtf8Path)?;
            Ok(RepoPath::parse(&p)?)
        })
        .transpose()
    };
    let mode = |side: Option<Side>| side.map(|(m, _)| Mode(u32::from(m.value())));
    let (old_mode, new_mode) = match change {
        Change::Added => (None, mode(new)),
        Change::Deleted => (mode(old), None),
        // Only stated when it changed, as in git's output.
        _ if mode(old) != mode(new) => (mode(old), mode(new)),
        _ => (None, None),
    };
    let old_bytes = content(repo, old)?;
    let new_bytes = content(repo, new)?;
    let binary = is_binary(&old_bytes) || is_binary(&new_bytes);
    let hunks = if binary || old_bytes == new_bytes {
        Vec::new()
    } else {
        hunks(&old_bytes, &new_bytes)
    };
    Ok(Some(FileDiff {
        old_path: path(old_path)?,
        new_path: path(new_path)?,
        change,
        old_mode,
        new_mode,
        binary,
        hunks,
    }))
}

/// A side's content as git diffs it: the blob, a link's target, or for a submodule
/// the `Subproject commit <id>` line git shows.
fn content(repo: &gix::Repository, side: Option<Side>) -> Result<Vec<u8>> {
    let Some((mode, id)) = side else {
        return Ok(Vec::new());
    };
    if mode.is_commit() {
        return Ok(format!("Subproject commit {id}\n").into_bytes());
    }
    Ok(repo
        .find_blob(id)
        .map_err(|e| object_error(id, e))?
        .data
        .clone())
}

/// git's rule (`buffer_is_binary`): a NUL byte in the first 8000 bytes. `.gitattributes`
/// overrides aren't read: they're repository-controlled and only change presentation.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// Hunks with git's defaults: Myers with git's slider heuristics, 3 lines of context.
fn hunks(old: &[u8], new: &[u8]) -> Vec<Hunk> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Myers, &input);
    UnifiedDiff::new(
        &diff,
        &input,
        Collect::default(),
        ContextSize::symmetrical(3),
    )
    .consume()
    .expect("collecting into memory doesn't fail")
}

#[derive(Default)]
struct Collect(Vec<Hunk>);

impl ConsumeHunk for Collect {
    type Out = Vec<Hunk>;

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        // git numbers an empty side from the line before the change.
        let start = |start: u32, len: u32| {
            if len == 0 {
                start.saturating_sub(1)
            } else {
                start
            }
        };
        let lines = lines
            .iter()
            .map(|(kind, bytes)| {
                let missing_newline = !bytes.ends_with(b"\n");
                let text = bytes.strip_suffix(b"\n").unwrap_or(bytes);
                Line {
                    kind: match kind {
                        DiffLineKind::Context => LineKind::Context,
                        DiffLineKind::Add => LineKind::Added,
                        DiffLineKind::Remove => LineKind::Removed,
                    },
                    // Non-UTF-8 content is shown with replacement characters; line
                    // structure (and so line numbers) is unchanged.
                    text: String::from_utf8_lossy(text).into_owned(),
                    missing_newline,
                }
            })
            .collect();
        self.0.push(Hunk {
            old_start: start(header.before_hunk_start, header.before_hunk_len),
            old_lines: header.before_hunk_len,
            new_start: start(header.after_hunk_start, header.after_hunk_len),
            new_lines: header.after_hunk_len,
            section: String::new(),
            lines,
        });
        Ok(())
    }

    fn finish(self) -> Self::Out {
        self.0
    }
}
