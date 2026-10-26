// SPDX-License-Identifier: AGPL-3.0-or-later

//! Context for a change (INDEXING §20–21): the changed code first, then what the graph
//! connects it to, then what lexical search finds for its names, fused by rank and packed
//! into a budget, with every item saying why it's there and what was left out.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use gitcko_diff::{ChangeSet, LineKind};
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
        let mut symbols_of: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, s) in self.graph.symbols.iter().enumerate() {
            symbols_of
                .entry(self.graph.files[s.file].path.as_str())
                .or_default()
                .push(i);
        }
        // 1. Anchors: chunks overlapping the changed lines of each file. The changed
        // symbols are the innermost declarations those lines touch, so an edit inside a
        // module's nested function seeds the function, not the module.
        let mut anchors: Vec<usize> = Vec::new();
        let mut changed: BTreeSet<usize> = BTreeSet::new();
        for file in &change.files {
            let Some(path) = &file.new_path else { continue };
            let chunks = chunks_of.get(path.as_str()).map_or(&[][..], Vec::as_slice);
            let symbols = symbols_of.get(path.as_str()).map_or(&[][..], Vec::as_slice);
            for (first, last) in file.hunks.iter().flat_map(changed_lines) {
                let overlaps = |(a, b): (usize, usize)| a <= last && first <= b;
                let mut here = Vec::new();
                for &c in chunks {
                    if overlaps(self.chunks[c].span.lines()) {
                        here.push(c);
                        if !anchors.contains(&c) {
                            anchors.push(c);
                        }
                    }
                }
                let touched: Vec<usize> = symbols
                    .iter()
                    .copied()
                    .filter(|&s| overlaps(self.graph.symbols[s].span.lines()))
                    .collect();
                let inside = |outer: usize, inner: usize| {
                    let (o, i) = (
                        &self.graph.symbols[outer].span,
                        &self.graph.symbols[inner].span,
                    );
                    outer != inner
                        && o.start_byte() <= i.start_byte()
                        && i.end_byte() <= o.end_byte()
                };
                let innermost: Vec<usize> = touched
                    .iter()
                    .copied()
                    .filter(|&s| !touched.iter().any(|&t| inside(s, t)))
                    .collect();
                if innermost.is_empty() {
                    // Lines outside any declaration (a doc comment, an attribute): the
                    // chunk's own symbol.
                    changed.extend(here.iter().filter_map(|&c| self.chunks[c].symbol));
                } else {
                    changed.extend(innermost);
                }
            }
        }

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
                    // Several neighbours in one chunk (functions of one small module)
                    // rank it once, at the nearest.
                    if let Some(&c) = symbol_chunk.get(&neighbour)
                        && !graph_ranked.contains(&c)
                    {
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
        // Matches past `lexical_hits` are left out, and counted like any other cut.
        let (lexical_ranked, lexical_cut): (Vec<usize>, usize) = if names.is_empty() {
            (Vec::new(), 0)
        } else {
            self.lexical
                .search(
                    &Query::new(QueryMode::Tokens, names.join(" ")),
                    budget.lexical_hits,
                    0,
                )
                .map(|r| {
                    let ids: Vec<usize> = r.hits.iter().map(|h| h.id as usize).collect();
                    let cut = r.total.saturating_sub(ids.len());
                    (ids, cut)
                })
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
        let mut omitted = lexical_cut;
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
        // Every symbol a packed chunk holds, nested ones included.
        let packed: BTreeSet<usize> = items.iter().map(|i| i.chunk).collect();
        let included: BTreeSet<usize> = symbol_chunk
            .iter()
            .filter(|(_, c)| packed.contains(c))
            .map(|(&s, _)| s)
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

/// The new-side line ranges a hunk changes: each added line, and the boundary where lines
/// were removed (the lines either side of it). Unchanged context lines aren't changes.
fn changed_lines(hunk: &gitcko_diff::Hunk) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    // A hunk adding nothing (`+N,0`) starts after line N.
    let mut line = hunk.new_start as usize + usize::from(hunk.new_lines == 0);
    for l in &hunk.lines {
        match l.kind {
            LineKind::Added => {
                out.push((line, line));
                line += 1;
            }
            LineKind::Removed => out.push((line.saturating_sub(1).max(1), line)),
            LineKind::Context => line += 1,
        }
    }
    out
}
