// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a file is, before anything reads it as code (INDEXING §9): its language, its
//! role in the repository, and whether it is binary, generated or vendored. Later, the
//! Tree-sitter adapters for supported languages live here too (INDEXING §10).
//!
//! Classification looks only at the path and the first bytes. It can be wrong (a `.rs`
//! file full of binary data, a generated file without a marker), so it guides how a file
//! is indexed and ranked; it never claims more than that (§9.2).

mod classify;

pub use classify::{Classification, FileRole, Language, classify};
