// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ids in the documented `gitcko.review/1` formats (CLI_SPEC §6).

use std::fmt;
use std::str::FromStr;

/// An id that doesn't match its documented format, e.g. a review id typed wrong on
/// the command line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind} id {value:?}: expected {expected}")]
pub struct IdError {
    kind: &'static str,
    value: String,
    expected: &'static str,
}

/// One review run: `rv_` and 26 characters of Crockford base32.
///
/// A UUIDv7 printed the way a ULID is. Both are 48 bits of milliseconds then random
/// bits, so ids sort roughly by creation time and need no coordination; `uuid` is the
/// widely used crate, so we print it ourselves instead of adding a ULID crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ReviewId(u128);

/// The ULID alphabet: no I, L, O or U, so ids survive being read aloud or retyped.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

impl ReviewId {
    /// A fresh id. UUIDv7 puts the time first, so ids sort by creation time, like a
    /// ULID.
    #[allow(clippy::new_without_default)] // a default would hide that each call is new
    pub fn new() -> ReviewId {
        ReviewId(uuid::Uuid::now_v7().as_u128())
    }
}

impl fmt::Display for ReviewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 26 five-bit digits = 130 bits; the first digit carries the top 3 bits only.
        let digits: String = (0..26)
            .map(|i| CROCKFORD[((self.0 >> (125 - 5 * i)) & 31) as usize] as char)
            .collect();
        write!(f, "rv_{digits}")
    }
}

impl FromStr for ReviewId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || IdError {
            kind: "review",
            value: s.to_string(),
            expected: "rv_ and 26 Crockford base32 characters",
        };
        let digits = s
            .strip_prefix("rv_")
            .filter(|d| d.len() == 26)
            .ok_or_else(err)?;
        let mut value: u128 = 0;
        for (i, byte) in digits.bytes().enumerate() {
            let digit = CROCKFORD.iter().position(|&c| c == byte).ok_or_else(err)? as u128;
            // A first digit above 7 would need more than 128 bits.
            if i == 0 && digit > 7 {
                return Err(err());
            }
            value = (value << 5) | digit;
        }
        Ok(ReviewId(value))
    }
}

/// A finding: `f_` and 16 lowercase hex digits of a stable fingerprint of the issue
/// (REVIEW_ENGINE §16 decides what goes into the fingerprint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FindingId(u64);

/// A question comment: `q_` and 16 lowercase hex digits (REVIEW_RULES §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QuestionId(u64);

impl FindingId {
    /// The same issue always gets the same fingerprint (REVIEW_ENGINE §16), so its id
    /// stays stable across reviews of the same PR and lifecycle can be tracked.
    pub fn from_fingerprint(fingerprint: u64) -> FindingId {
        FindingId(fingerprint)
    }
}

impl QuestionId {
    /// Stable across reviews for the same reason as [`FindingId::from_fingerprint`].
    pub fn from_fingerprint(fingerprint: u64) -> QuestionId {
        QuestionId(fingerprint)
    }
}

impl fmt::Display for FindingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "f_{:016x}", self.0)
    }
}

impl fmt::Display for QuestionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "q_{:016x}", self.0)
    }
}

impl FromStr for FindingId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_hex_id(s, "f_", "finding").map(FindingId)
    }
}

impl FromStr for QuestionId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_hex_id(s, "q_", "question").map(QuestionId)
    }
}

/// Exactly 16 lowercase hex digits after `prefix`, so each id has one spelling.
fn parse_hex_id(s: &str, prefix: &str, kind: &'static str) -> Result<u64, IdError> {
    let hex = s
        .strip_prefix(prefix)
        .filter(|h| h.len() == 16 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    hex.and_then(|h| u64::from_str_radix(h, 16).ok())
        .ok_or_else(|| IdError {
            kind,
            value: s.to_string(),
            expected: "the prefix and 16 lowercase hex digits",
        })
}
