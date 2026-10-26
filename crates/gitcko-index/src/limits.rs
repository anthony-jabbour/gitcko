// SPDX-License-Identifier: AGPL-3.0-or-later

/// Every bound the index works within (INDEXING §35.2). Each has a finite default,
/// because an unset limit must never mean "unlimited" against a hostile repository
/// (TM-38); hitting one marks the affected scope `partial` with a reason, never a crash.
/// Fields are added with the code that enforces them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexLimits {
    /// Files indexed per commit, taken in path order so the cut is the same every time.
    pub max_files: usize,
    /// Total bytes of file content read per commit.
    pub max_repo_bytes: u64,
    /// Largest file parsed into symbols; bigger files are searched as text only.
    pub max_parse_bytes: u64,
    /// Largest file read at all; bigger files keep only their metadata.
    pub max_lexical_bytes: u64,
    /// `.gitcko/` files (config and rules) read per commit. Past this, or past
    /// `max_config_bytes`, no repository config is used at all: half a cascade could be
    /// looser than the whole.
    pub max_config_files: usize,
    /// Total bytes of `.gitcko/` files read per commit.
    pub max_config_bytes: u64,
    /// Bounds on parsing one file (time, nodes, depth, symbols, references).
    pub parse: gitcko_syntax::ParseLimits,
    /// Bounds on chunking (sizes, chunks per file).
    pub chunks: crate::ChunkPolicy,
    /// Chunks per revision; past it, the rest are left out and lexical coverage says so.
    pub max_chunks: usize,
}

impl Default for IndexLimits {
    fn default() -> IndexLimits {
        IndexLimits {
            max_files: 200_000,
            max_repo_bytes: 2 << 30,
            max_parse_bytes: 1 << 20,
            max_lexical_bytes: 4 << 20,
            max_config_files: 1_000,
            max_config_bytes: 8 << 20,
            parse: gitcko_syntax::ParseLimits::default(),
            chunks: crate::ChunkPolicy::default(),
            max_chunks: 2_000_000,
        }
    }
}
