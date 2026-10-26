// SPDX-License-Identifier: AGPL-3.0-or-later

//! Gitcko's repository index (INDEXING): everything a review needs to know about the code
//! beyond the diff, built from one immutable commit.
//!
//! It never executes anything from the repository (IX-031, ADR-9): files are read as git
//! objects through `gitcko-git`, never from a checkout, and parsed by Tree-sitter.
//! Every result says how complete it is ([`gitcko_domain::Coverage`]), so a gap in the
//! index can't be mistaken for an absence in the code.

mod build;
mod chunk;
mod discover;
mod limits;
mod query;
mod retrieve;
mod store;

pub use build::{
    INDEX_SCHEMA_VERSION, ParsedFile, Revision, RevisionCoverage, Versions, build, validate,
};
pub use chunk::{
    CHUNK_SCHEMA_VERSION, Chunk, ChunkKind, ChunkPolicy, ChunkSymbol, FileContext, Segment,
    chunk_code, chunk_text,
};
pub use discover::{Disposition, IndexError, Manifest, ManifestEntry, discover};
pub use limits::IndexLimits;
pub use query::{
    CodeHit, CodeQuery, CodeResults, CodeSearchMode, Overview, Relations, SymbolMatches,
    TestCandidates, TestEvidence,
};
pub use retrieve::{BundleCoverage, ContextBundle, ContextItem, RetrievalBudget, Source};
pub use store::{PublishStep, Store, StoreError, StoredRevision};
