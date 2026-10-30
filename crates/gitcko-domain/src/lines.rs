// SPDX-License-Identifier: AGPL-3.0-or-later

/// Which side of a diff a line number counts on: the base (`Old`) or the head (`New`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    Old,
    New,
}

/// A 1-based, inclusive line range on one side: what a finding or a piece of evidence
/// cites. Unlike [`SourceSpan`](crate::SourceSpan) (byte offsets and 0-based rows from
/// the parser) it is in the units a reader and the code host use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineRange {
    side: Side,
    start: u32,
    end: u32,
}

/// Why a range was refused. Value-free: the numbers come from model output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LineRangeError {
    #[error("line numbers start at 1")]
    ZeroLine,
    #[error("a line range can't end before it starts")]
    EndBeforeStart,
}

impl LineRange {
    /// Checks the range once, so code holding one never sees line 0 or a reversed range.
    pub fn new(side: Side, start: u32, end: u32) -> Result<LineRange, LineRangeError> {
        if start == 0 {
            return Err(LineRangeError::ZeroLine);
        }
        if end < start {
            return Err(LineRangeError::EndBeforeStart);
        }
        Ok(LineRange { side, start, end })
    }

    pub fn side(&self) -> Side {
        self.side
    }

    pub fn start(&self) -> u32 {
        self.start
    }

    pub fn end(&self) -> u32 {
        self.end
    }

    /// True when `line` is inside the range, ends included.
    pub fn contains(&self, line: u32) -> bool {
        (self.start..=self.end).contains(&line)
    }

    /// True when both ranges share a line on the same side: the dedup test for "same
    /// place" (REVIEW_ENGINE §13).
    pub fn overlaps(&self, other: &LineRange) -> bool {
        self.side == other.side && self.start <= other.end && other.start <= self.end
    }
}
