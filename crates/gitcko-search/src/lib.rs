// SPDX-License-Identifier: AGPL-3.0-or-later

//! Local lexical search (INDEXING §18, IX-011): a Tantivy inverted index over code
//! chunks, embedded in the process, so local Gitcko needs no database or daemon (IX-025).
//!
//! This crate knows documents and fields, not symbols or chunks: the index crate decides
//! what goes in a document. Lexical search is a first-class retrieval source, not only a
//! fallback (IX-018), but it isn't the authority on exact text: a literal search that must
//! not miss a match reads the source itself (§18.4).

mod index;
mod tokenize;

pub use index::{
    Hit, LEXICAL_SCHEMA_VERSION, LexicalDoc, LexicalIndex, MAX_RESULTS, Query, QueryMode, Results,
    SearchError,
};
pub use tokenize::{CodeToken, TokenKind, code_tokens};
