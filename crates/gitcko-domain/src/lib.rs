// SPDX-License-Identifier: AGPL-3.0-or-later

//! Gitcko's validated core types. Each one checks its invariant when it is built, so
//! code that holds one doesn't check again: a [`RepoPath`] can't escape the repository,
//! a [`Confidence`] is a real number from 0 to 1, and ids have one spelling.
//!
//! The index types ([`Coverage`], [`ContentHash`], [`SourceSpan`]) are shared by every
//! indexing crate and later by the review engine, so they live here rather than in any
//! one of them. They derive serde because the local index cache stores them as JSON.
//!
//! The serialized forms live in `gitcko-protocol`. The review engine, which holds both,
//! converts its validated values into them when it assembles a result; nothing reads a
//! review back into these types yet (that arrives with `gitcko review show`, Phase 5).

mod confidence;
mod coverage;
mod hash;
mod id;
mod lines;
mod path;
mod span;

pub use confidence::{Band, Confidence, ConfidenceError};
pub use coverage::{Coverage, Reason, ReasonError};
pub use hash::{ContentHash, HashError};
pub use id::{FindingId, IdError, QuestionId, ReviewId};
pub use lines::{LineRange, LineRangeError, Side};
pub use path::{PathError, PathProblem, RepoPath};
pub use span::{Position, SourceSpan, SpanError};
