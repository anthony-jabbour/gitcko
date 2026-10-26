// SPDX-License-Identifier: AGPL-3.0-or-later

//! Building one revision of the index (INDEXING §5, §26): discover → parse → graph →
//! chunks → lexical index → validate. Nothing is exposed until validation passes, so an
//! inconsistent build can't be read (IX-022).

use std::collections::{BTreeMap, HashMap};

use gitcko_domain::{Coverage, Reason, RepoPath};
use gitcko_git::{CommitId, ObjectId, Repo};
use gitcko_graph::{
    EdgeKind, Endpoint, FileInput, GRAPH_SCHEMA_VERSION, Graph, GraphInput, Target,
};
use gitcko_search::{LEXICAL_SCHEMA_VERSION, LexicalDoc, LexicalIndex};
use gitcko_syntax::{Extraction, FileRole, Language, SymbolKind, adapter};

use crate::{
    CHUNK_SCHEMA_VERSION, Chunk, ChunkPolicy, ChunkSymbol, Disposition, FileContext, IndexError,
    IndexLimits, Manifest, chunk_code, chunk_text, discover,
};

/// Changes whenever the revision's overall layout changes (INDEXING §6).
pub const INDEX_SCHEMA_VERSION: u32 = 1;

/// Every schema and extractor version a revision was built with (INDEXING §6). Two
/// revisions with different versions are never mixed or reused.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Versions {
    pub index: u32,
    pub graph: u32,
    pub chunk: u32,
    pub lexical: u32,
    /// Each language adapter's version, sorted.
    pub adapters: Vec<String>,
}

impl Versions {
    /// The versions this build of Gitcko writes.
    pub fn current() -> Versions {
        let mut adapters: Vec<String> = [Language::Rust]
            .into_iter()
            .filter_map(adapter)
            .map(|a| a.version().to_string())
            .collect();
        adapters.sort();
        Versions {
            index: INDEX_SCHEMA_VERSION,
            graph: GRAPH_SCHEMA_VERSION,
            chunk: CHUNK_SCHEMA_VERSION,
            lexical: LEXICAL_SCHEMA_VERSION,
            adapters,
        }
    }

    /// One short label for all of them, `idx1.1.1.2-rust-1+tree-sitter-rust-0.24`: what a
    /// review records as `versions.index`, and what names the cache folder, so a change
    /// in any version means a fresh index.
    pub fn label(&self) -> String {
        format!(
            "idx{}.{}.{}.{}-{}",
            self.index,
            self.graph,
            self.chunk,
            self.lexical,
            self.adapters.join("_")
        )
    }
}

/// How complete each part of a revision is (INDEXING §27.1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevisionCoverage {
    pub discovery: Coverage,
    /// Parsing and symbol extraction across parsed files.
    pub parsing: Coverage,
    /// Chunks: complete unless a chunk limit cut some.
    pub chunking: Coverage,
    /// Lexical search: complete unless files with text couldn't be read or chunked.
    pub lexical: Coverage,
    /// Semantic search: unavailable without an embedding provider (IX-013).
    pub semantic: Coverage,
    /// Per edge kind, from the graph. Not stored: the graph is re-resolved on load.
    #[serde(skip)]
    pub graph: BTreeMap<EdgeKind, Coverage>,
}

/// One file's parse result in a revision.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParsedFile {
    /// Index into the manifest's entries.
    pub entry: usize,
    pub extraction: Extraction,
}

/// A complete, validated index of one commit.
pub struct Revision {
    pub commit: String,
    pub manifest: Manifest,
    pub parsed: Vec<ParsedFile>,
    pub graph: Graph,
    /// Chunk `i` is lexical document `i`.
    pub chunks: Vec<Chunk>,
    pub lexical: LexicalIndex,
    pub coverage: RevisionCoverage,
    pub versions: Versions,
}

/// Builds and validates the revision for `commit`. Reads only git objects; runs nothing.
pub fn build(repo: &Repo, commit: CommitId, limits: &IndexLimits) -> Result<Revision, IndexError> {
    build_reusing(repo, commit, limits, &HashMap::new())
}

/// Builds `commit` like [`build`], but reuses `previous`'s parse of any file whose content
/// it parsed completely, wherever that file now lives (INDEXING §23). Everything after
/// parsing (graph, chunks, search index) is rebuilt in full, so the result equals a clean
/// build by construction, with no dependency tracking to get wrong (§23.5–23.6). Nothing
/// is reused if `previous` was built with other versions. `previous` must have been built
/// with the same `limits`: a parse under other limits could have come out differently.
pub fn build_incremental(
    repo: &Repo,
    commit: CommitId,
    limits: &IndexLimits,
    previous: &Revision,
) -> Result<Revision, IndexError> {
    let mut reuse = HashMap::new();
    if previous.versions == Versions::current() {
        for p in &previous.parsed {
            // A partial parse may have been cut short by a timeout: parse it again.
            let complete = p.extraction.coverage.is_complete()
                && p.extraction.reference_coverage.is_complete();
            if let Some(e) = previous.manifest.entries.get(p.entry)
                && complete
            {
                reuse.insert((e.object, e.language), &p.extraction);
            }
        }
    }
    build_reusing(repo, commit, limits, &reuse)
}

/// A parse to reuse, by the file's content and language.
type Reuse<'a> = HashMap<(ObjectId, Option<Language>), &'a Extraction>;

fn build_reusing(
    repo: &Repo,
    commit: CommitId,
    limits: &IndexLimits,
    reuse: &Reuse<'_>,
) -> Result<Revision, IndexError> {
    let manifest = discover(repo, commit, limits)?;
    let mut texts: HashMap<RepoPath, String> = HashMap::new();
    let mut parsed = Vec::new();
    let mut unreadable = 0u64;
    let mut partial_parses = 0u64;
    for (i, entry) in manifest.entries.iter().enumerate() {
        if !matches!(
            entry.disposition,
            Disposition::Parse | Disposition::LexicalOnly
        ) {
            continue;
        }
        let Some(path) = &entry.repo_path else {
            continue;
        };
        // Discovery already checked size and UTF-8; a failure now means the object went
        // bad between the two reads, and costs only this file.
        let Some(text) = repo
            .read_blob_id(entry.object)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
        else {
            unreadable += 1;
            continue;
        };
        if entry.disposition == Disposition::Parse
            && let Some(a) = entry.language.and_then(adapter)
        {
            let extraction = match reuse.get(&(entry.object, entry.language)) {
                Some(x) => (*x).clone(),
                None => a.extract(&text, &limits.parse),
            };
            if !extraction.coverage.is_complete() {
                partial_parses += 1;
            }
            parsed.push(ParsedFile {
                entry: i,
                extraction,
            });
        }
        texts.insert(path.clone(), text);
    }

    let graph = resolve_graph(&manifest, &parsed, |p| texts.get(p).cloned());

    let (chunks, chunk_coverage) = make_chunks(&manifest, &parsed, &graph, &texts, limits);
    let docs = lexical_docs(&manifest, &graph, &chunks).ok_or(IndexError::Invalid {
        check: "chunk_source",
    })?;
    let lexical = LexicalIndex::build(None, docs).map_err(|_| IndexError::Invalid {
        check: "lexical_build",
    })?;
    let partial = |code: &str, omitted: u64| Coverage::Partial {
        reason: Reason::new(code).expect("valid code"),
        omitted: Some(omitted),
    };
    let not_searchable = manifest
        .entries
        .iter()
        .filter(|e| {
            matches!(
                e.disposition,
                Disposition::ExcludedResourceLimit | Disposition::Unavailable
            )
        })
        .count() as u64
        + unreadable;
    let coverage = RevisionCoverage {
        discovery: manifest.coverage.clone(),
        parsing: if partial_parses > 0 {
            partial("file_partial", partial_parses)
        } else {
            Coverage::Complete {}
        },
        lexical: if not_searchable > 0 {
            partial("files_not_searchable", not_searchable)
        } else {
            chunk_coverage.clone()
        },
        chunking: chunk_coverage,
        semantic: Coverage::Unavailable {
            reason: Reason::new("embeddings_disabled").expect("valid code"),
        },
        graph: graph.coverage.clone(),
    };
    let revision = Revision {
        commit: commit.to_string(),
        manifest,
        parsed,
        graph,
        chunks,
        lexical,
        coverage,
        versions: Versions::current(),
    };
    validate(&revision, |path| texts.get(path).cloned())?;
    Ok(revision)
}

/// Resolves the graph from the parsed files and every `Cargo.toml`'s text (read through
/// `text`). Deterministic, so the store re-resolves rather than storing it.
pub(crate) fn resolve_graph(
    manifest: &Manifest,
    parsed: &[ParsedFile],
    text: impl Fn(&RepoPath) -> Option<String>,
) -> Graph {
    let cargo: Vec<(RepoPath, String)> = manifest
        .entries
        .iter()
        .filter(|e| matches!(e.disposition, Disposition::Parse | Disposition::LexicalOnly))
        .filter_map(|e| e.repo_path.clone())
        .filter(|p| p.as_str() == "Cargo.toml" || p.as_str().ends_with("/Cargo.toml"))
        .filter_map(|p| Some((p.clone(), text(&p)?)))
        .collect();
    gitcko_graph::build(&GraphInput {
        files: parsed
            .iter()
            .filter_map(|p| {
                Some(FileInput {
                    path: manifest.entries[p.entry].repo_path.clone()?,
                    extraction: &p.extraction,
                })
            })
            .collect(),
        manifests: cargo.iter().map(|(p, t)| (p.clone(), t.as_str())).collect(),
    })
}

/// Chunks every readable text file: code around its symbols, the rest by structure.
fn make_chunks(
    manifest: &Manifest,
    parsed: &[ParsedFile],
    graph: &Graph,
    texts: &HashMap<RepoPath, String>,
    limits: &IndexLimits,
) -> (Vec<Chunk>, Coverage) {
    let by_entry: HashMap<usize, &ParsedFile> = parsed.iter().map(|p| (p.entry, p)).collect();
    let graph_file: HashMap<&str, usize> = graph
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.path.as_str(), i))
        .collect();
    let qualified: HashMap<(usize, usize), &str> = graph
        .symbols
        .iter()
        .map(|s| ((s.file, s.local), s.qualified_name.as_str()))
        .collect();
    let mut by_file: HashMap<usize, Vec<usize>> = HashMap::new();
    for (gi, s) in graph.symbols.iter().enumerate() {
        by_file.entry(s.file).or_default().push(gi);
    }
    let mut chunks = Vec::new();
    let mut omitted = 0u64;
    // A file cut short, or skipped once the revision's budget ran out: more was left out
    // than was counted.
    let mut uncounted = false;
    for (i, entry) in manifest.entries.iter().enumerate() {
        let Some(path) = &entry.repo_path else {
            continue;
        };
        let Some(text) = texts.get(path) else {
            continue;
        };
        let room = limits.max_chunks.saturating_sub(chunks.len());
        if room == 0 {
            uncounted = true;
            break;
        }
        let file = FileContext {
            path,
            language: entry.language,
            role: entry.role,
        };
        // Never plan more for one file than the revision can still hold.
        let policy = ChunkPolicy {
            max_chunks_per_file: limits.chunks.max_chunks_per_file.min(room),
            ..limits.chunks.clone()
        };
        let (file_chunks, coverage) = match (by_entry.get(&i), graph_file.get(path.as_str())) {
            (Some(p), Some(&gf)) => {
                let x = &p.extraction;
                let parent_name = |local: usize| qualified.get(&(gf, local)).copied();
                let symbols: Vec<ChunkSymbol<'_>> = by_file
                    .get(&gf)
                    .into_iter()
                    .flatten()
                    .map(|&gi| (gi, &graph.symbols[gi]))
                    .filter(|(_, s)| {
                        let native = &x.symbols[s.local].native_kind;
                        s.kind != SymbolKind::Field && native != "enum_variant"
                    })
                    .map(|(gi, s)| ChunkSymbol {
                        graph_index: gi,
                        kind: s.kind,
                        span: s.span,
                        qualified_name: &s.qualified_name,
                        signature: x.symbols[s.local].signature.as_deref(),
                        parent: x.symbols[s.local].parent.and_then(parent_name),
                    })
                    .collect();
                chunk_code(&file, text, &symbols, &policy)
            }
            _ => chunk_text(&file, text, &policy),
        };
        match coverage {
            Coverage::Partial {
                omitted: Some(n), ..
            } => omitted += n,
            Coverage::Partial { omitted: None, .. } => uncounted = true,
            _ => {}
        }
        chunks.extend(file_chunks);
    }
    let coverage = if uncounted || omitted > 0 {
        Coverage::Partial {
            reason: Reason::new("chunk_limit").expect("valid code"),
            omitted: (!uncounted).then_some(omitted),
        }
    } else {
        Coverage::Complete {}
    };
    (chunks, coverage)
}

/// One lexical document per chunk, with the symbol's names in their own fields.
/// One search document per chunk, chunk `i` as document `i`; `None` if a chunk names a
/// file or symbol the revision doesn't have.
pub(crate) fn lexical_docs(
    manifest: &Manifest,
    graph: &Graph,
    chunks: &[Chunk],
) -> Option<Vec<LexicalDoc>> {
    let entry_of: HashMap<&str, usize> = manifest
        .entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| Some((e.repo_path.as_ref()?.as_str(), i)))
        .collect();
    chunks
        .iter()
        .enumerate()
        .map(|(id, c)| {
            let entry = &manifest.entries[*entry_of.get(c.path.as_str())?];
            let symbol = match c.symbol {
                Some(s) => Some(graph.symbols.get(s)?),
                None => None,
            };
            Some(LexicalDoc {
                id: id as u64,
                path: c.path.as_str().to_string(),
                language: entry.language.map(language_name),
                role: role_name(entry.role),
                kind: c.kind.as_str().to_string(),
                symbol_key: symbol.map(|s| s.key.0.clone()),
                name: symbol.map(|s| s.name.clone()),
                qualified_name: symbol.map(|s| s.qualified_name.clone()),
                signature: None,
                content: c.text.clone(),
            })
        })
        .collect()
}

/// A language as search documents and filters spell it (`rust`).
pub(crate) fn language_name(language: Language) -> String {
    format!("{language:?}").to_lowercase()
}

/// A file role as search documents and filters spell it (`dependency_lock`).
pub(crate) fn role_name(role: FileRole) -> String {
    let debug = format!("{role:?}");
    let mut out = String::new();
    for (i, c) in debug.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Checks a revision is coherent before anything may read it (INDEXING §26): graph edges
/// point at real things, every chunk lies inside its file and matches the source byte for
/// byte, every chunk has its lexical document, and the versions are complete. `source`
/// gives a file's exact text at the commit. A failure names the check, never content.
pub fn validate(
    revision: &Revision,
    source: impl Fn(&RepoPath) -> Option<String>,
) -> Result<(), IndexError> {
    let fail = |check| Err(IndexError::Invalid { check });
    let g = &revision.graph;
    let ok = |e: &Endpoint| match e {
        Endpoint::File(f) => *f < g.files.len(),
        Endpoint::Module(c, _) => *c < g.crates.len(),
        Endpoint::Symbol(s) => *s < g.symbols.len(),
    };
    for edge in &g.edges {
        let targets_ok = match &edge.to {
            Target::Resolved(t) => ok(t),
            Target::Candidates(c) => c.iter().all(ok),
            Target::Unresolved(_) => true,
        };
        if !ok(&edge.from) || !targets_ok || edge.file >= g.files.len() {
            return fail("graph_endpoint");
        }
    }
    let mut cache: HashMap<&RepoPath, Option<String>> = HashMap::new();
    for chunk in &revision.chunks {
        let text = cache
            .entry(&chunk.path)
            .or_insert_with(|| source(&chunk.path));
        let Some(text) = text else {
            return fail("chunk_source");
        };
        let Some(slice) = text.get(chunk.span.start_byte()..chunk.span.end_byte()) else {
            return fail("chunk_bounds");
        };
        let hash = gitcko_domain::ContentHash::of(slice.as_bytes());
        if hash != chunk.content_hash || chunk.text != slice {
            return fail("chunk_hash");
        }
        if chunk.symbol.is_some_and(|s| s >= g.symbols.len()) {
            return fail("chunk_symbol");
        }
    }
    if revision.lexical.len() != revision.chunks.len() as u64 {
        return fail("lexical_count");
    }
    // Document `i` must be chunk `i`'s, field for field, not just one per chunk.
    let Some(expected) = lexical_docs(&revision.manifest, g, &revision.chunks) else {
        return fail("chunk_source");
    };
    let Ok(stored) = revision.lexical.digests() else {
        return fail("lexical_docs");
    };
    let expected: Vec<(u64, String)> = expected
        .iter()
        .enumerate()
        .map(|(i, doc)| (i as u64, doc.digest()))
        .collect();
    if stored != expected {
        return fail("lexical_docs");
    }
    if revision.versions != Versions::current() {
        return fail("versions");
    }
    Ok(())
}
