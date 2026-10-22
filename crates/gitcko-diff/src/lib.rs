// SPDX-License-Identifier: AGPL-3.0-or-later

//! The change under review as data: files, hunks and lines, parsed from git's unified
//! diff format, plus mapping between old and new line numbers.
//!
//! `gitcko-git` builds a [`ChangeSet`] from two commits; [`parse`] builds the same type
//! from diff text (fixtures, uploaded patches). Every path goes through
//! [`RepoPath`], so a patch can't name a file outside the repository.
//!
//! The format rules follow git's own diff output and `git apply`
//! (`Documentation/diff-generate-patch.adoc` and `apply.c` in git.git).

mod parse;
mod render;

use gitcko_domain::RepoPath;

pub use parse::{ParseError, parse};

/// Every file a change touches, in diff order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub files: Vec<FileDiff>,
}

/// One file's change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// `None` for an added file.
    pub old_path: Option<RepoPath>,
    /// `None` for a deleted file.
    pub new_path: Option<RepoPath>,
    pub change: Change,
    /// Set only when the diff states it: a new or deleted file, or a mode change.
    pub old_mode: Option<Mode>,
    pub new_mode: Option<Mode>,
    /// Git reported the content as binary; there are no hunks.
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Deleted,
    Modified,
    /// `similarity` is git's percentage, 0–100.
    Renamed {
        similarity: u8,
    },
    Copied {
        similarity: u8,
    },
}

/// A git file mode, e.g. `0o100644` (file), `0o100755` (executable), `0o120000`
/// (symlink), `0o160000` (submodule).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Mode(pub u32);

impl std::fmt::Debug for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Mode({:06o})", self.0)
    }
}

/// `@@ -old_start,old_lines +new_start,new_lines @@ section`. For an empty side
/// (`lines == 0`), `start` is the line *before* the change, as in git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    /// The function or section name git shows after the second `@@`, or empty.
    pub section: String,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    /// Without the leading marker or the `\n`. A CRLF file keeps its `\r`.
    pub text: String,
    /// Followed by `\ No newline at end of file`.
    pub missing_newline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// Which version of the file a line number refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Old,
    New,
}

impl Hunk {
    /// First line number on each side that this hunk covers.
    fn first(&self, side: Side) -> u32 {
        let (start, lines) = match side {
            Side::Old => (self.old_start, self.old_lines),
            Side::New => (self.new_start, self.new_lines),
        };
        if lines == 0 { start + 1 } else { start }
    }
}

impl FileDiff {
    /// The old line a new line came from, or `None` if the change added it.
    pub fn old_line(&self, new_line: u32) -> Option<u32> {
        self.map(Side::New, new_line)
    }

    /// Where an old line ended up, or `None` if the change removed it.
    pub fn new_line(&self, old_line: u32) -> Option<u32> {
        self.map(Side::Old, old_line)
    }

    /// Whether `line` on `side` is inside a hunk, i.e. shown in the diff (changed or
    /// context), which is where a code host accepts an inline comment.
    pub fn in_hunk(&self, side: Side, line: u32) -> bool {
        self.hunks.iter().any(|h| {
            let lines = if side == Side::Old {
                h.old_lines
            } else {
                h.new_lines
            };
            (h.first(side)..h.first(side) + lines).contains(&line)
        })
    }

    fn map(&self, from: Side, line: u32) -> Option<u32> {
        let to = if from == Side::Old {
            Side::New
        } else {
            Side::Old
        };
        // An added file has no old version and a deleted one no new version.
        let missing = match self.change {
            Change::Added => Some(Side::Old),
            Change::Deleted => Some(Side::New),
            _ => None,
        };
        if missing == Some(to) || missing == Some(from) {
            return None;
        }
        // Lines outside every hunk are unchanged and shift by however many lines the
        // hunks before them added or removed.
        let mut shift: i64 = 0;
        for hunk in &self.hunks {
            let (mut at_from, mut at_to) = (hunk.first(from), hunk.first(to));
            if line < at_from {
                break;
            }
            for l in &hunk.lines {
                let on_from = l.kind == LineKind::Context
                    || (l.kind == LineKind::Removed) == (from == Side::Old);
                let on_to = l.kind == LineKind::Context || !on_from;
                if on_from && at_from == line {
                    return (l.kind == LineKind::Context).then_some(at_to);
                }
                at_from += u32::from(on_from);
                at_to += u32::from(on_to);
            }
            shift = i64::from(at_to) - i64::from(at_from);
        }
        u32::try_from(i64::from(line) + shift).ok()
    }
}
