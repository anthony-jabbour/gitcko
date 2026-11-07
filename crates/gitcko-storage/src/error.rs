// SPDX-License-Identifier: AGPL-3.0-or-later

/// Why a store operation failed. Never names a path, pid, or bytes from disk.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The repo id would not be a safe folder name.
    #[error("a repository id must be 1 to 64 characters of a-z, 0-9, _ and -")]
    InvalidRepoId,
    /// The review id is not `rv_` plus 26 Crockford characters.
    #[error("a review id must be rv_ and 26 ULID characters")]
    InvalidReviewId,
    /// No JSON file for that review id.
    #[error("no such review in the store")]
    NotFound,
    /// The file is not a `gitcko.review/1` document whose id matches its name.
    #[error("the stored review is corrupt ({check})")]
    Corrupt {
        /// Which check failed (`json` or `id`). Never the file's bytes.
        check: &'static str,
    },
    /// A filesystem call failed. `op` is a fixed verb, never a path.
    #[error("the store couldn't {op}")]
    Io {
        /// What we were trying to do, e.g. `write the store`.
        op: &'static str,
    },
}
