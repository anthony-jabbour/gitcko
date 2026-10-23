// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a file is, before anything reads it as code (INDEXING §9): its language, its
//! role in the repository, and whether it is binary, generated or vendored. Then, for
//! languages with an adapter, what it declares and refers to (INDEXING §10), read from a
//! Tree-sitter syntax tree. Nothing here compiles, expands macros or runs anything from
//! the repository (IX-031): it only reads text.
//!
//! Classification looks only at the path and the first bytes. It can be wrong (a `.rs`
//! file full of binary data, a generated file without a marker), so it guides how a file
//! is indexed and ranked; it never claims more than that (§9.2).

mod adapter;
mod classify;
#[cfg(feature = "lang-rust")]
mod rust;

pub use adapter::{
    Extension, Extraction, Implementation, Import, LanguageAdapter, ModuleDecl, ParseLimits,
    RefKind, Reference, Scope, Symbol, SymbolKind, Visibility, adapter,
};
pub use classify::{Classification, FileRole, Language, classify};
