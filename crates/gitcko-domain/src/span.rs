// SPDX-License-Identifier: AGPL-3.0-or-later

/// A place in a file, measured the way Tree-sitter measures it: rows and columns count
/// from 0, and a column is a byte offset within its row (INDEXING §11.4). Kept apart from
/// the 1-based, side-aware line ranges findings use (Phase 4) because the two have
/// different invariants.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Position {
    /// Line number, counting from 0.
    pub row: usize,
    /// Byte offset within the line, counting from 0 (not a character count).
    pub column: usize,
}

/// A range of a file's bytes, with the row/column of each end. Built only through
/// [`SourceSpan::new`] (also when read back from a cache file), so `start` is never
/// after `end`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "RawSpan")]
pub struct SourceSpan {
    start_byte: usize,
    end_byte: usize,
    start: Position,
    end: Position,
}

/// A span whose start comes after its end, in bytes or in position. Carries no source.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a source span can't end before it starts")]
pub struct SpanError;

impl SourceSpan {
    /// A span from `start_byte` to `end_byte` (exclusive), at `start`..`end`.
    pub fn new(
        start_byte: usize,
        end_byte: usize,
        start: Position,
        end: Position,
    ) -> Result<SourceSpan, SpanError> {
        if start_byte > end_byte || start > end {
            return Err(SpanError);
        }
        Ok(SourceSpan {
            start_byte,
            end_byte,
            start,
            end,
        })
    }

    /// First byte of the span.
    pub fn start_byte(&self) -> usize {
        self.start_byte
    }

    /// One past the last byte, so `end_byte - start_byte` is the length.
    pub fn end_byte(&self) -> usize {
        self.end_byte
    }

    /// Row and column of the first byte.
    pub fn start(&self) -> Position {
        self.start
    }

    /// Row and column just past the last byte.
    pub fn end(&self) -> Position {
        self.end
    }

    /// The 1-based first and last line numbers, as an editor shows them.
    pub fn lines(&self) -> (usize, usize) {
        (self.start.row + 1, self.end.row + 1)
    }
}

/// The unchecked shape on disk; turned into a [`SourceSpan`] only through `new`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpan {
    start_byte: usize,
    end_byte: usize,
    start: Position,
    end: Position,
}

impl TryFrom<RawSpan> for SourceSpan {
    type Error = SpanError;
    fn try_from(raw: RawSpan) -> Result<Self, Self::Error> {
        SourceSpan::new(raw.start_byte, raw.end_byte, raw.start, raw.end)
    }
}
