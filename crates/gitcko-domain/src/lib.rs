// SPDX-License-Identifier: AGPL-3.0-or-later

//! Gitcko's validated core types. Each one checks its invariant when it is built, so
//! code that holds one doesn't check again: a [`RepoPath`] can't escape the repository,
//! a [`Confidence`] is a real number from 0 to 1, and ids have one spelling.
//!
//! The serialized forms live in `gitcko-protocol`; conversions to and from it are added
//! with the first code that needs them (the review engine, Phase 4).

mod confidence;
mod id;
mod path;

pub use confidence::{Band, Confidence, ConfidenceError};
pub use id::{FindingId, IdError, QuestionId, ReviewId};
pub use path::{PathError, PathProblem, RepoPath};
