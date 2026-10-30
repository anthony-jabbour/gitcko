// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which declarations a change touches (REVIEW_ENGINE §6): head symbols from the index,
//! kept when their span meets a hunk's new-side lines.
// ponytail: head side only, so a deleted or renamed item isn't seen as changed here. The
// diff still reaches the planner. Parse the base blobs if eval misses point at it.

use gitcko_diff::{FileDiff, LineKind};
use gitcko_domain::RepoPath;
use gitcko_syntax::Visibility;

use crate::snapshot::Snapshot;

/// How many callers to count before saying "many".
const CALLER_LIMIT: usize = 20;

/// One changed declaration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChangedSymbol {
    pub path: String,
    pub qualified_name: String,
    pub public: bool,
    pub callers: usize,
}

fn touched(file: &FileDiff, start: u32, end: u32) -> bool {
    file.hunks.iter().any(|hunk| {
        let mut new_line = hunk.new_start;
        hunk.lines.iter().any(|line| {
            let hit = match line.kind {
                LineKind::Added => new_line >= start && new_line <= end,
                // A removal sits between two new lines; count it against the next one.
                LineKind::Removed => new_line >= start && new_line <= end,
                LineKind::Context => false,
            };
            if line.kind != LineKind::Removed {
                new_line += 1;
            }
            hit
        })
    })
}

fn is_public(snapshot: &Snapshot<'_>, path: &RepoPath, local: usize) -> bool {
    let index = &snapshot.index;
    index
        .parsed
        .iter()
        .find(|p| index.manifest.entries[p.entry].path == path.as_str())
        .and_then(|p| p.extraction.symbols.get(local))
        .is_some_and(|s| s.visibility == Visibility::Public)
}

/// The changed declarations in stable order.
pub fn changed(snapshot: &Snapshot<'_>) -> Vec<ChangedSymbol> {
    let graph = &snapshot.index.graph;
    let allowed = snapshot.investigated();
    let mut out = Vec::new();
    for (at, symbol) in graph.symbols.iter().enumerate() {
        let Some(node) = graph.files.get(symbol.file) else {
            continue;
        };
        let Some(file) = snapshot.change.files.iter().find(|f| {
            f.new_path
                .as_ref()
                .is_some_and(|p| *p == node.path && allowed.contains(&p))
        }) else {
            continue;
        };
        let (start, end) = symbol.span.lines();
        if !touched(file, start as u32, end as u32) {
            continue;
        }
        out.push(ChangedSymbol {
            path: node.path.as_str().to_string(),
            qualified_name: symbol.qualified_name.clone(),
            public: is_public(snapshot, &node.path, symbol.local),
            callers: snapshot.index.find_callers(at, CALLER_LIMIT).resolved.len(),
        });
    }
    out.sort();
    out
}
