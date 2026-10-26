// SPDX-License-Identifier: AGPL-3.0-or-later

//! The read-only questions the review engine asks the index (INDEXING §22). Every answer
//! carries its coverage, so "nothing found" is never mistaken for "nothing exists" when
//! the index couldn't see everything (IX-024), and says when it was cut short (IX-033).

use gitcko_domain::{Coverage, Reason, RepoPath, SourceSpan};
use gitcko_git::Repo;
use gitcko_graph::{EdgeKind, Endpoint, Target};
use gitcko_search::{MAX_RESULTS, Query, QueryMode, SearchError};
use gitcko_syntax::SymbolKind;

use crate::{Disposition, IndexError, Revision};

/// Declarations matching a name (§22.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolMatches {
    /// Indexes into the graph's symbols: every plausible declaration, best first (exact
    /// qualified name, then exact name, then qualified-name suffix), never one picked.
    pub symbols: Vec<usize>,
    pub coverage: Coverage,
    pub truncated: bool,
}

/// Relationships around one symbol (§22.3–22.5), resolved and candidate kept apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relations {
    /// Edges (indexes into the graph's edges) that settle on the symbol.
    pub resolved: Vec<usize>,
    /// Edges where the symbol is one of several candidates.
    pub candidates: Vec<usize>,
    /// For callees: calls from the symbol to names the repository doesn't declare.
    pub unresolved: Vec<usize>,
    /// For this edge kind across the revision: `[]` under anything but `complete` isn't
    /// proof of none.
    pub coverage: Coverage,
    pub truncated: bool,
}

/// Why a test is offered as related (§22.6). Related, never "this behaviour is tested".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestEvidence {
    /// A `tests` edge: the test calls it.
    Calls,
    /// In the same module as the symbol.
    SameModule,
    /// Its name contains the symbol's name.
    Name,
}

/// Test candidates for a symbol, best evidence first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestCandidates {
    pub tests: Vec<(usize, TestEvidence)>,
    pub coverage: Coverage,
    pub truncated: bool,
}

/// How [`Revision::search_code`] matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeSearchMode {
    /// Lexical search, through the Tantivy index.
    Lexical(QueryMode),
    /// Exact text, read from the commit's files: never misses a match the index's
    /// tokenization would hide (§18.4).
    Literal,
}

/// A code search (§22.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeQuery {
    pub text: String,
    pub mode: CodeSearchMode,
    pub language: Option<String>,
    pub role: Option<String>,
    pub path_prefix: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

/// One place a search matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeHit {
    pub path: RepoPath,
    pub span: SourceSpan,
    /// The chunk it's in, for lexical hits.
    pub chunk: Option<usize>,
}

/// The matches, and what they cover (§22.1): complete, truncated, or a scope that
/// couldn't all be searched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeResults {
    pub hits: Vec<CodeHit>,
    /// Matches in all, when known (lexical); literal scans stop counting at the page.
    pub total: Option<usize>,
    pub truncated: bool,
    pub coverage: Coverage,
}

/// Deterministic facts about the repository (§22.8), with no model involved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    pub files: usize,
    pub parsed_files: usize,
    pub symbols: usize,
    pub chunks: usize,
    /// Crate target names, sorted.
    pub crates: Vec<String>,
    /// Files per language, sorted by language name.
    pub languages: Vec<(String, usize)>,
}

impl Revision {
    /// Every declaration named `name` (a plain or qualified name), optionally only in
    /// `path`. Ambiguity is the answer's to keep: same-named declarations all come back.
    pub fn find_symbol(&self, name: &str, path: Option<&RepoPath>, limit: usize) -> SymbolMatches {
        let g = &self.graph;
        let suffix = format!("::{name}");
        let mut ranked: Vec<(u8, usize)> = g
            .symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| path.is_none_or(|p| g.files[s.file].path == *p))
            .filter_map(|(i, s)| {
                let rank = if s.qualified_name == name {
                    0
                } else if s.name == name {
                    1
                } else if s.qualified_name.ends_with(&suffix) {
                    2
                } else {
                    return None;
                };
                Some((rank, i))
            })
            .collect();
        ranked.sort();
        let (symbols, truncated) = page(ranked.into_iter().map(|(_, i)| i).collect(), limit, 0);
        SymbolMatches {
            symbols,
            coverage: self.declaration_coverage(path),
            truncated,
        }
    }

    /// Parsing coverage, made partial by source files in scope that were never parsed
    /// (too big, unreadable, over a limit): a declaration there can't be seen.
    fn declaration_coverage(&self, path: Option<&RepoPath>) -> Coverage {
        let parsed: std::collections::BTreeSet<usize> =
            self.parsed.iter().map(|p| p.entry).collect();
        let unparsed = self
            .manifest
            .entries
            .iter()
            .enumerate()
            .filter(|(i, e)| {
                path.is_none_or(|p| e.path == p.as_str())
                    && !parsed.contains(i)
                    && (e.language.is_some_and(|l| l.has_adapter())
                        || matches!(
                            e.disposition,
                            Disposition::ExcludedResourceLimit | Disposition::Unavailable
                        ))
            })
            .count() as u64;
        if unparsed == 0 {
            return self.coverage.parsing.clone();
        }
        Coverage::Partial {
            reason: Reason::new("files_not_parsed").expect("valid code"),
            // Already partial parses are a second gap, of unknown size.
            omitted: self.coverage.parsing.is_complete().then_some(unparsed),
        }
    }

    /// Where `symbol` is named as a type or path, or its macro invoked (§22.3).
    pub fn find_references(&self, symbol: usize, limit: usize) -> Relations {
        self.incoming(EdgeKind::References, &Endpoint::Symbol(symbol), limit)
    }

    /// Who calls `symbol` (§22.4).
    pub fn find_callers(&self, symbol: usize, limit: usize) -> Relations {
        self.incoming(EdgeKind::Calls, &Endpoint::Symbol(symbol), limit)
    }

    /// What `symbol` calls (§22.4), including calls the repository can't settle.
    pub fn find_callees(&self, symbol: usize, limit: usize) -> Relations {
        let from = Endpoint::Symbol(symbol);
        let mut resolved = Vec::new();
        let mut candidates = Vec::new();
        let mut unresolved = Vec::new();
        for (i, e) in self.graph.edges.iter().enumerate() {
            if e.kind != EdgeKind::Calls || e.from != from {
                continue;
            }
            match &e.to {
                Target::Resolved(_) => resolved.push(i),
                Target::Candidates(_) => candidates.push(i),
                Target::Unresolved(_) => unresolved.push(i),
            }
        }
        let (resolved, a) = page(resolved, limit, 0);
        let (candidates, b) = page(candidates, limit, 0);
        let (unresolved, c) = page(unresolved, limit, 0);
        Relations {
            resolved,
            candidates,
            unresolved,
            coverage: self.coverage.graph[&EdgeKind::Calls].clone(),
            truncated: a || b || c,
        }
    }

    /// Files importing `target`, a symbol or module (§22.5).
    pub fn find_importers(&self, target: &Endpoint, limit: usize) -> Relations {
        self.incoming(EdgeKind::Imports, target, limit)
    }

    /// Tests related to `symbol` (§22.6): tests that call it, then tests in its module,
    /// then tests named after it.
    pub fn find_tests(&self, symbol: usize, limit: usize) -> TestCandidates {
        let g = &self.graph;
        let target = &g.symbols[symbol];
        let home = self.modules_of(symbol);
        let mut found: Vec<(usize, TestEvidence)> = Vec::new();
        let mut add = |s: usize, why| {
            if !found.iter().any(|(t, _)| *t == s) {
                found.push((s, why));
            }
        };
        for e in &g.edges {
            if e.kind == EdgeKind::Tests
                && e.to == Target::Resolved(Endpoint::Symbol(symbol))
                && let Endpoint::Symbol(t) = e.from
            {
                add(t, TestEvidence::Calls);
            }
        }
        let is_test = |s: &gitcko_graph::GraphSymbol| s.kind == SymbolKind::Test;
        // The symbol's own module, or a module directly inside it (`mod tests`), in the
        // same crate target.
        for (i, s) in g.symbols.iter().enumerate() {
            if is_test(s)
                && self.modules_of(i).iter().any(|(c, m)| {
                    home.iter().any(|(hc, hm)| {
                        c == hc && (m == hm || m.split_last().is_some_and(|(_, p)| p == hm))
                    })
                })
            {
                add(i, TestEvidence::SameModule);
            }
        }
        let name = target.name.to_lowercase();
        for (i, s) in g.symbols.iter().enumerate() {
            if is_test(s) && s.name.to_lowercase().contains(&name) {
                add(i, TestEvidence::Name);
            }
        }
        let (tests, truncated) = page(found, limit, 0);
        TestCandidates {
            tests,
            coverage: self.coverage.graph[&EdgeKind::Tests].clone(),
            truncated,
        }
    }

    /// Searches code (§22.1). Lexical modes use the index; literal mode reads the
    /// commit's text files through `repo`, bounded by `max_scan_bytes`.
    pub fn search_code(
        &self,
        repo: &Repo,
        query: &CodeQuery,
        max_scan_bytes: u64,
    ) -> Result<CodeResults, IndexError> {
        match query.mode {
            CodeSearchMode::Lexical(mode) => self.lexical_search(query, mode),
            CodeSearchMode::Literal => self.literal_search(repo, query, max_scan_bytes),
        }
    }

    /// Deterministic counts (§22.8).
    pub fn repo_overview(&self) -> Overview {
        let mut crates: Vec<String> = self.graph.crates.iter().map(|c| c.name.clone()).collect();
        crates.sort();
        crates.dedup();
        let mut languages: std::collections::BTreeMap<String, usize> = Default::default();
        for e in &self.manifest.entries {
            if let Some(l) = e.language {
                *languages
                    .entry(format!("{l:?}").to_lowercase())
                    .or_default() += 1;
            }
        }
        Overview {
            files: self.manifest.entries.len(),
            parsed_files: self.parsed.len(),
            symbols: self.graph.symbols.len(),
            chunks: self.chunks.len(),
            crates,
            languages: languages.into_iter().collect(),
        }
    }

    /// Every (crate target, module path) a symbol lives in: its file's module in each
    /// target, plus the inline `mod` blocks around it.
    fn modules_of(&self, symbol: usize) -> Vec<(usize, Vec<String>)> {
        let g = &self.graph;
        let s = &g.symbols[symbol];
        let mut inline: Vec<&gitcko_graph::GraphSymbol> = g
            .symbols
            .iter()
            .enumerate()
            .filter(|&(i, m)| {
                i != symbol
                    && m.file == s.file
                    && m.kind == SymbolKind::Module
                    && m.span.start_byte() <= s.span.start_byte()
                    && s.span.end_byte() <= m.span.end_byte()
            })
            .map(|(_, m)| m)
            .collect();
        inline.sort_by_key(|m| m.span.start_byte());
        g.files[s.file]
            .memberships
            .iter()
            .map(|(c, module)| {
                let mut path = module.clone();
                path.extend(inline.iter().map(|m| m.name.clone()));
                (*c, path)
            })
            .collect()
    }

    fn incoming(&self, kind: EdgeKind, target: &Endpoint, limit: usize) -> Relations {
        let mut resolved = Vec::new();
        let mut candidates = Vec::new();
        for (i, e) in self.graph.edges.iter().enumerate() {
            if e.kind != kind {
                continue;
            }
            match &e.to {
                Target::Resolved(t) if t == target => resolved.push(i),
                Target::Candidates(c) if c.contains(target) => candidates.push(i),
                _ => {}
            }
        }
        let (resolved, a) = page(resolved, limit, 0);
        let (candidates, b) = page(candidates, limit, 0);
        Relations {
            resolved,
            candidates,
            unresolved: Vec::new(),
            coverage: self.coverage.graph[&kind].clone(),
            truncated: a || b,
        }
    }

    fn lexical_search(
        &self,
        query: &CodeQuery,
        mode: QueryMode,
    ) -> Result<CodeResults, IndexError> {
        let q = Query {
            mode,
            text: query.text.clone(),
            language: query.language.clone(),
            role: query.role.clone(),
            path_prefix: query.path_prefix.clone(),
        };
        let results = self
            .lexical
            .search(&q, query.limit, query.offset)
            .map_err(|e| match e {
                SearchError::EmptyQuery => IndexError::EmptyQuery,
                _ => IndexError::Invalid {
                    check: "lexical_read",
                },
            })?;
        let hits = results
            .hits
            .iter()
            .map(|h| {
                let c = &self.chunks[h.id as usize];
                CodeHit {
                    path: c.path.clone(),
                    span: c.span,
                    chunk: Some(h.id as usize),
                }
            })
            .collect();
        Ok(CodeResults {
            hits,
            total: Some(results.total),
            truncated: results.truncated,
            coverage: self.coverage.lexical.clone(),
        })
    }

    fn literal_search(
        &self,
        repo: &Repo,
        query: &CodeQuery,
        max_scan_bytes: u64,
    ) -> Result<CodeResults, IndexError> {
        if query.text.is_empty() {
            return Err(IndexError::EmptyQuery);
        }
        let limit = query.limit.min(MAX_RESULTS);
        let mut hits = Vec::new();
        let mut skipped = 0u64;
        let mut scanned = 0u64;
        let (mut page_full, mut budget_spent) = (false, false);
        let mut seen = 0usize;
        // The same filters, spelled the same way, as a lexical search. A file whose
        // language isn't known (unreadable) may be in scope.
        let in_scope = |e: &crate::ManifestEntry| {
            query
                .path_prefix
                .as_ref()
                .is_none_or(|p| e.path.starts_with(p.as_str()))
                && (query.language.is_none()
                    || e.language.is_none()
                    || query.language == e.language.map(crate::build::language_name))
                && query
                    .role
                    .as_ref()
                    .is_none_or(|r| *r == crate::build::role_name(e.role))
        };
        'files: for e in &self.manifest.entries {
            let Some(path) = &e.repo_path else { continue };
            if !matches!(e.disposition, Disposition::Parse | Disposition::LexicalOnly)
                || !in_scope(e)
            {
                continue;
            }
            let size = e.size.unwrap_or(0);
            if scanned + size > max_scan_bytes {
                budget_spent = true;
                break;
            }
            scanned += size;
            let Some(text) = repo
                .read_blob_id(e.object)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
            else {
                skipped += 1;
                continue;
            };
            let lines = line_starts(&text);
            for (at, m) in text.match_indices(query.text.as_str()) {
                seen += 1;
                if seen <= query.offset {
                    continue;
                }
                if hits.len() == limit {
                    page_full = true;
                    break 'files;
                }
                hits.push(CodeHit {
                    path: path.clone(),
                    span: span_at(&lines, at, at + m.len()),
                    chunk: None,
                });
            }
        }
        let excluded = self
            .manifest
            .entries
            .iter()
            .filter(|e| {
                matches!(
                    e.disposition,
                    Disposition::ExcludedResourceLimit | Disposition::Unavailable
                ) && in_scope(e)
            })
            .count() as u64;
        let coverage = if skipped + excluded > 0 {
            Coverage::Partial {
                reason: Reason::new("files_not_searchable").expect("valid code"),
                // A spent scan budget leaves a second gap, of unknown size.
                omitted: (!budget_spent).then_some(skipped + excluded),
            }
        } else if budget_spent {
            Coverage::Partial {
                reason: Reason::new("scan_limit").expect("valid code"),
                omitted: None,
            }
        } else {
            Coverage::Complete {}
        };
        Ok(CodeResults {
            hits,
            total: None,
            truncated: page_full || budget_spent,
            coverage,
        })
    }
}

/// The first `limit` of `items` from `offset`, and whether more remain.
fn page<T>(items: Vec<T>, limit: usize, offset: usize) -> (Vec<T>, bool) {
    let limit = limit.min(MAX_RESULTS);
    let more = items.len() > offset + limit;
    (items.into_iter().skip(offset).take(limit).collect(), more)
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    starts
}

fn span_at(lines: &[usize], a: usize, b: usize) -> SourceSpan {
    let pos = |byte: usize| {
        let row = lines.partition_point(|&s| s <= byte) - 1;
        gitcko_domain::Position {
            row,
            column: byte - lines[row],
        }
    };
    SourceSpan::new(a, b, pos(a), pos(b)).expect("a <= b")
}
