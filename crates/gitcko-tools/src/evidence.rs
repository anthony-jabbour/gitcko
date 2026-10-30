// SPDX-License-Identifier: AGPL-3.0-or-later

//! The evidence ledger: every range of source a tool actually returned in this run, with
//! a digest of the exact text. A candidate finding may cite only these ids, so a claim
//! can't rest on a read that failed or on lines the model invented (REVIEW_ENGINE §10,
//! §13).

use gitcko_domain::{ContentHash, LineRange, RepoPath};

/// One observed range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    /// `e1`, `e2`, …: what the model cites.
    pub id: String,
    pub path: RepoPath,
    pub range: LineRange,
    /// SHA-256 of `text`.
    pub digest: ContentHash,
    /// The lines exactly as returned, joined with `\n`. Repository text: untrusted.
    pub text: String,
}

/// Everything observed so far in one run, in the order it was observed.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    entries: Vec<Evidence>,
}

impl Ledger {
    /// Records `text` as observed at `path`/`range` and returns its id. The same range
    /// with the same text keeps its first id, so re-reading doesn't multiply evidence.
    pub(crate) fn record(&mut self, path: &RepoPath, range: LineRange, text: String) -> String {
        let digest = ContentHash::of(text.as_bytes());
        if let Some(e) = self
            .entries
            .iter()
            .find(|e| e.path == *path && e.range == range && e.digest == digest)
        {
            return e.id.clone();
        }
        let id = format!("e{}", self.entries.len() + 1);
        self.entries.push(Evidence {
            id: id.clone(),
            path: path.clone(),
            range,
            digest,
            text,
        });
        id
    }

    /// The evidence with this id, if a tool produced it in this run.
    pub fn get(&self, id: &str) -> Option<&Evidence> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Every observed range, in order.
    pub fn entries(&self) -> &[Evidence] {
        &self.entries
    }
}
