// SPDX-License-Identifier: AGPL-3.0-or-later

//! Context for a change (INDEXING §20–21): the changed code first, then what the graph
//! connects it to, then what lexical search finds for its names, fused by rank and packed
//! into a budget, with every item saying why it's there and what was left out.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use gitcko_diff::ChangeSet;
use gitcko_domain::{ContentHash, Coverage, Reason, RepoPath, SourceSpan};
use gitcko_graph::{EdgeKind, Endpoint, Target};
use gitcko_search::{Query, QueryMode};

use crate::{ChunkKind, Revision};

/// The reciprocal-rank fusion constant: the usual starting value (INDEXING §20.3), a
/// default to tune with evals, not a guarantee.
const RRF_K: f64 = 60.0;

/// A fused candidate: chunk, score, and every source that found it with its rank.
type Fused = (usize, f64, Vec<(Source, usize)>);

/// How much context a retrieval may return (INDEXING §20.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalBudget {
    pub max_items: usize,
    /// Total bytes of item text (headers included).
    pub max_bytes: usize,
    /// Graph hops followed from the changed symbols.
    pub graph_depth: usize,
    /// Lexical hits considered.
    pub lexical_hits: usize,
}

impl Default for RetrievalBudget {
    fn default() -> RetrievalBudget {
        RetrievalBudget {
            max_items: 40,
            max_bytes: 120_000,
            graph_depth: 2,
            lexical_hits: 20,
        }
    }
}

/// Where a context item came from (INDEXING §20.1), strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    /// It overlaps the changed lines.
    Anchor,
    /// The graph connects it to a changed symbol.
    Graph,
    /// Lexical search for the changed symbols' names found it.
    Lexical,
}

impl Source {
    /// Weights in the order INDEXING §20.3 gives: exact anchors over structure over
    /// lexical. Starting values, to be tuned by evals.
    fn weight(self) -> f64 {
        match self {
            Source::Anchor => 4.0,
            Source::Graph => 2.0,
            Source::Lexical => 1.0,
        }
    }
}

/// One piece of context (INDEXING §21).
#[derive(Debug, Clone, PartialEq)]
pub struct ContextItem {
    /// Index into the revision's chunks.
    pub chunk: usize,
    pub kind: ChunkKind,
    pub path: RepoPath,
    pub span: SourceSpan,
    pub symbol_key: Option<String>,
    pub content_hash: ContentHash,
    /// The chunk's metadata header and its text.
    pub text: String,
    /// Every way it was found, with its rank there (1 = first): the provenance §20.4
    /// keeps when duplicates merge.
    pub sources: Vec<(Source, usize)>,
    pub score: f64,
}

/// Coverage of each retrieval source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleCoverage {
    /// From the graph's calls coverage: partial means callers or callees may be missing.
    pub structural: Coverage,
    pub lexical: Coverage,
    /// Unavailable while embeddings are off (IX-013).
    pub semantic: Coverage,
}

/// The context for one change (INDEXING §21).
#[derive(Debug, Clone, PartialEq)]
pub struct ContextBundle {
    /// The commit and index versions the context comes from.
    pub commit: String,
    pub index_versions: String,
    pub items: Vec<ContextItem>,
    /// Graph edges (indexes into the revision's) between the items' symbols.
    pub relationships: Vec<usize>,
    pub coverage: BundleCoverage,
    /// Candidates found but left out by the budget.
    pub omitted: usize,
    pub truncated: bool,
}

impl Revision {
    /// Gathers context for `change` within `budget`. The changed code always comes first;
    /// anything cut is counted, never silently dropped (§20.5).
    pub fn retrieve(&self, change: &ChangeSet, budget: &RetrievalBudget) -> ContextBundle {
        let chunks_of = self.chunks_by_path();
        // 1. Anchors: chunks overlapping the changed lines of each file.
        let mut anchors: Vec<usize> = Vec::new();
        for file in &change.files {
            let Some(path) = &file.new_path else { continue };
            let Some(chunks) = chunks_of.get(path.as_str()) else {
                continue;
            };
            for hunk in &file.hunks {
                let (first, last) = (
                    hunk.new_start as usize,
                    (hunk.new_start + hunk.new_lines.max(1) - 1) as usize,
                );
                for &c in chunks {
                    let (a, b) = self.chunks[c].span.lines();
                    if a <= last && first <= b && !anchors.contains(&c) {
                        anchors.push(c);
                    }
                }
            }
        }
        let changed: BTreeSet<usize> = anchors
            .iter()
            .filter_map(|&c| self.chunks[c].symbol)
            .collect();

        // 2. Graph neighbourhood, nearest first.
        let symbol_chunk = self.symbol_chunks();
        let mut graph_ranked: Vec<usize> = Vec::new();
        let mut seen: BTreeSet<usize> = changed.clone();
        let mut queue: VecDeque<(usize, usize)> = changed.iter().map(|&s| (s, 0)).collect();
        while let Some((s, depth)) = queue.pop_front() {
            if depth >= budget.graph_depth {
                continue;
            }
            for neighbour in self.neighbours(s) {
                if seen.insert(neighbour) {
                    if let Some(&c) = symbol_chunk.get(&neighbour) {
                        graph_ranked.push(c);
                    }
                    queue.push_back((neighbour, depth + 1));
                }
            }
        }

        // 3. Lexical: the changed symbols' names.
        let names: Vec<&str> = changed
            .iter()
            .map(|&s| self.graph.symbols[s].name.as_str())
            .collect();
        let lexical_ranked: Vec<usize> = if names.is_empty() {
            Vec::new()
        } else {
            self.lexical
                .search(
                    &Query::new(QueryMode::Tokens, names.join(" ")),
                    budget.lexical_hits,
                    0,
                )
                .map(|r| r.hits.iter().map(|h| h.id as usize).collect())
                .unwrap_or_default()
        };

        // 4. Weighted reciprocal-rank fusion, duplicates merged with their provenance.
        let mut fused: BTreeMap<usize, (f64, Vec<(Source, usize)>)> = BTreeMap::new();
        for (source, list) in [
            (Source::Anchor, &anchors),
            (Source::Graph, &graph_ranked),
            (Source::Lexical, &lexical_ranked),
        ] {
            for (rank, &c) in list.iter().enumerate() {
                let entry = fused.entry(c).or_default();
                entry.0 += source.weight() / (RRF_K + rank as f64 + 1.0);
                entry.1.push((source, rank + 1));
            }
        }
        let mut order: Vec<Fused> = fused
            .into_iter()
            .map(|(c, (score, sources))| (c, score, sources))
            .collect();
        // Anchors are reserved first (§20.5 step 1); then by score, ties by chunk order.
        order.sort_by(|a, b| {
            let anchor = |x: &Fused| x.2.iter().any(|(s, _)| *s == Source::Anchor);
            anchor(b)
                .cmp(&anchor(a))
                .then(b.1.total_cmp(&a.1))
                .then(a.0.cmp(&b.0))
        });

        // 5. Pack into the budget.
        let mut items = Vec::new();
        let mut bytes = 0;
        let mut omitted = 0;
        for (c, score, sources) in order {
            let chunk = &self.chunks[c];
            let text = format!("{}\n{}", chunk.header, chunk.text);
            if items.len() >= budget.max_items || bytes + text.len() > budget.max_bytes {
                omitted += 1;
                continue;
            }
            bytes += text.len();
            items.push(ContextItem {
                chunk: c,
                kind: chunk.kind,
                path: chunk.path.clone(),
                span: chunk.span,
                symbol_key: chunk.symbol.map(|s| self.graph.symbols[s].key.0.clone()),
                content_hash: chunk.content_hash,
                text,
                sources,
                score,
            });
        }
        let included: BTreeSet<usize> = items
            .iter()
            .filter_map(|i| self.chunks[i.chunk].symbol)
            .collect();
        let relationships = self
            .graph
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                matches!(
                    e.kind,
                    EdgeKind::Calls
                        | EdgeKind::References
                        | EdgeKind::Implements
                        | EdgeKind::Extends
                        | EdgeKind::Tests
                )
            })
            .filter(|(_, e)| match (&e.from, &e.to) {
                (Endpoint::Symbol(a), Target::Resolved(Endpoint::Symbol(b))) => {
                    included.contains(a) && included.contains(b)
                }
                _ => false,
            })
            .map(|(i, _)| i)
            .collect();
        ContextBundle {
            commit: self.commit.clone(),
            index_versions: self.versions.label(),
            items,
            relationships,
            coverage: BundleCoverage {
                structural: self.coverage.graph[&EdgeKind::Calls].clone(),
                lexical: self.coverage.lexical.clone(),
                semantic: Coverage::Unavailable {
                    reason: Reason::new("embeddings_disabled").expect("valid code"),
                },
            },
            truncated: omitted > 0,
            omitted,
        }
    }

    /// Symbols one resolved edge away: callers, callees, references either way,
    /// implementations and supertraits, tests (§20.2).
    fn neighbours(&self, symbol: usize) -> Vec<usize> {
        let me = Endpoint::Symbol(symbol);
        let mut out = Vec::new();
        for e in &self.graph.edges {
            if !matches!(
                e.kind,
                EdgeKind::Calls
                    | EdgeKind::References
                    | EdgeKind::Implements
                    | EdgeKind::Extends
                    | EdgeKind::Tests
            ) {
                continue;
            }
            match (&e.from, &e.to) {
                (from, Target::Resolved(Endpoint::Symbol(to))) if *from == me => out.push(*to),
                (Endpoint::Symbol(from), Target::Resolved(to)) if *to == me => out.push(*from),
                _ => {}
            }
        }
        out
    }

    /// Chunks per file, in order.
    fn chunks_by_path(&self) -> HashMap<&str, Vec<usize>> {
        let mut map: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, c) in self.chunks.iter().enumerate() {
            map.entry(c.path.as_str()).or_default().push(i);
        }
        map
    }

    /// The chunk holding each symbol: its own (the first, for a split one), or else the
    /// chunk where it starts, since items nested in a module or `impl` (tests, methods)
    /// are chunked with their parent. Chunks don't overlap, so there's at most one.
    fn symbol_chunks(&self) -> HashMap<usize, usize> {
        let mut map = HashMap::new();
        for (i, c) in self.chunks.iter().enumerate() {
            if let Some(s) = c.symbol {
                map.entry(s).or_insert(i);
            }
        }
        let chunks_of = self.chunks_by_path();
        for (s, sym) in self.graph.symbols.iter().enumerate() {
            if map.contains_key(&s) {
                continue;
            }
            let path = self.graph.files[sym.file].path.as_str();
            let start = sym.span.start_byte();
            if let Some(c) = chunks_of
                .get(path)
                .into_iter()
                .flatten()
                .copied()
                .find(|&c| {
                    let span = &self.chunks[c].span;
                    span.start_byte() <= start && start < span.end_byte()
                })
            {
                map.insert(s, c);
            }
        }
        map
    }
}
