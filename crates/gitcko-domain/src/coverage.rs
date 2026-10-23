// SPDX-License-Identifier: AGPL-3.0-or-later

/// How much of a scope an extractor actually covered (INDEXING §27). Every index result
/// carries one, so "no callers found" under [`Coverage::Partial`] can never be read as
/// "there are no callers", which only [`Coverage::Complete`] can say (IX-024).
///
/// The states are an enum with data rather than a state plus optional fields, so a
/// `Complete` with an omitted count, or a `Partial` with no reason, can't be built.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Coverage {
    /// The configured extractor processed the whole bounded scope (§27.2). It doesn't
    /// claim runtime semantics are known, only that nothing in scope was skipped.
    /// Braces rather than a unit variant: serde ignores `deny_unknown_fields` on a unit
    /// variant of a tagged enum, so `{"state":"complete","omitted":3}` would be accepted.
    Complete {},
    /// Useful facts were produced but some scope was left out (§27.3).
    Partial {
        reason: Reason,
        /// How many items were left out, when that is known.
        omitted: Option<u64>,
    },
    /// Neither presence nor absence can be justified, e.g. dynamic dispatch (§27.4).
    Unknown { reason: Reason },
    /// The capability didn't run or can't, e.g. embeddings disabled (§27.5).
    Unavailable { reason: Reason },
}

impl Coverage {
    /// True only for [`Coverage::Complete`]: the one state where an empty result means
    /// "none exist" (§27.6).
    pub fn is_complete(&self) -> bool {
        matches!(self, Coverage::Complete {})
    }
}

/// Why coverage isn't complete, as a fixed code such as `parse_timeout`. Only lowercase
/// ASCII letters, digits and `_`, at most 64 characters: a reason can't hold a path,
/// source text or anything else from a repository, so it is always safe to log and to
/// show (OBSERVABILITY §6).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Reason(String);

/// A reason code that isn't `[a-z0-9_]{1,64}`. The message doesn't repeat the value,
/// which could be anything.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a coverage reason must be 1 to 64 characters of a-z, 0-9 and _")]
pub struct ReasonError;

impl Reason {
    /// Checks the code's spelling. Reasons are written in code (`Reason::new("…")`), so
    /// an error here is a bug or a corrupt cache file, not user input.
    pub fn new(code: &str) -> Result<Reason, ReasonError> {
        let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_';
        if (1..=64).contains(&code.len()) && code.bytes().all(allowed) {
            Ok(Reason(code.to_string()))
        } else {
            Err(ReasonError)
        }
    }

    /// The code as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Reason {
    type Error = ReasonError;
    fn try_from(code: String) -> Result<Self, Self::Error> {
        Reason::new(&code)
    }
}

impl From<Reason> for String {
    fn from(reason: Reason) -> String {
        reason.0
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
