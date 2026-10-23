// SPDX-License-Identifier: AGPL-3.0-or-later

/// The SHA-256 of a file's bytes: the content identity the index caches by (INDEXING
/// §8.1). Git's blob ids are SHA-1, which isn't collision-resistant, so they can say
/// "unchanged" quickly but never stand in for this.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct ContentHash([u8; 32]);

/// Text that isn't 64 lowercase hex characters. The message doesn't repeat it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a content hash must be 64 lowercase hex characters")]
pub struct HashError;

impl ContentHash {
    /// Hashes `bytes`.
    pub fn of(bytes: &[u8]) -> ContentHash {
        use sha2::Digest;
        ContentHash(sha2::Sha256::digest(bytes).into())
    }

    /// The raw 32 bytes, for hashing a hash into a larger key.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::str::FromStr for ContentHash {
    type Err = HashError;
    fn from_str(hex: &str) -> Result<Self, Self::Err> {
        // One spelling only (lowercase), so two texts of the same hash always compare equal.
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            _ => Err(HashError),
        };
        let hex = hex.as_bytes();
        if hex.len() != 64 {
            return Err(HashError);
        }
        let mut bytes = [0u8; 32];
        for (byte, pair) in bytes.iter_mut().zip(hex.chunks_exact(2)) {
            *byte = nibble(pair[0])? << 4 | nibble(pair[1])?;
        }
        Ok(ContentHash(bytes))
    }
}

impl std::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// Hex, like `Display`: a 32-byte array is unreadable in test failures and logs.
impl std::fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ContentHash({self})")
    }
}

impl TryFrom<String> for ContentHash {
    type Error = HashError;
    fn try_from(hex: String) -> Result<Self, Self::Error> {
        hex.parse()
    }
}

impl From<ContentHash> for String {
    fn from(hash: ContentHash) -> String {
        hash.to_string()
    }
}
