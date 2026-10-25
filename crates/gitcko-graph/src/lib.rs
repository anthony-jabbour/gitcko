// SPDX-License-Identifier: AGPL-3.0-or-later

//! The code graph (INDEXING §11–13): every declaration with a stable identity, and the
//! relationships between them, `contains`, `declares`, `imports`, `references`, `calls`,
//! `implements`, `extends` and `tests` (IX-006).
//!
//! Relationships are resolved from syntax and the repository's own layout, never by
//! running a compiler or build tool (IX-031). So every edge says how it was resolved and
//! how sure that is (IX-007): a name the files can't settle stays a candidate or unknown,
//! never a guessed exact edge (§12.3), and coverage says what couldn't be seen (IX-024).

mod crates;
mod resolve;

use std::collections::{BTreeMap, HashMap};

use gitcko_domain::{Coverage, Position, RepoPath, SourceSpan};
use gitcko_syntax::{Extraction, RefKind, SymbolKind};

pub use crates::CrateUnit;

/// Changes whenever symbol keys or the graph's shape change, so a cached graph from an
/// older version is rebuilt (INDEXING §6).
pub const GRAPH_SCHEMA_VERSION: u32 = 1;

/// One parsed file going into the graph.
pub struct FileInput<'a> {
    pub path: RepoPath,
    pub extraction: &'a Extraction,
}

/// Everything the graph is built from: parsed files, and the text of every `Cargo.toml`
/// (read as data, never run), which says where crates and their roots are.
pub struct GraphInput<'a> {
    pub files: Vec<FileInput<'a>>,
    pub manifests: Vec<(RepoPath, &'a str)>,
}

/// A symbol's logical identity (INDEXING §11.3): a hash of its language, crate, module
/// path, kind, qualified name and discriminator, never its line numbers, so moving code
/// within a file keeps the key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolKey(pub String);

/// How far a key can be trusted across revisions (§11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStability {
    /// From the qualified name: survives edits elsewhere in the file.
    Stable,
    /// The file's module path was inferred (no crate root reaches it), so the key may
    /// change once one does.
    Inferred,
}

/// A file in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileNode {
    pub path: RepoPath,
    /// The crate target it belongs to (index into [`Graph::crates`]), when one reaches it.
    pub crate_unit: Option<usize>,
    /// Its module path inside that crate (`["shapes", "circle"]`), or a guess from the path.
    pub module: Vec<String>,
    pub module_inferred: bool,
}

/// A declaration in the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSymbol {
    pub key: SymbolKey,
    pub stability: KeyStability,
    /// Index into [`Graph::files`].
    pub file: usize,
    /// Index into that file's `Extraction::symbols`.
    pub local: usize,
    pub kind: SymbolKind,
    pub name: String,
    /// `crate::module::Type::name`, with the crate's own name.
    pub qualified_name: String,
    pub discriminator: Option<String>,
    pub span: SourceSpan,
    pub in_test: bool,
}

/// The eight V1 relationship kinds (IX-006, §12.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    Contains,
    Declares,
    Imports,
    References,
    Calls,
    Implements,
    Extends,
    Tests,
}

/// One end of an edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Endpoint {
    /// Index into [`Graph::files`].
    File(usize),
    /// A module: crate unit and path.
    Module(usize, Vec<String>),
    /// Index into [`Graph::symbols`].
    Symbol(usize),
}

/// Where an edge points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// One target, settled.
    Resolved(Endpoint),
    /// Several possible targets the syntax can't choose between; all are kept.
    Candidates(Vec<Endpoint>),
    /// No target in the repository: an external crate, a macro-made name, or a name the
    /// files don't declare. The path is kept as written.
    Unresolved(Vec<String>),
}

/// How an edge was established (INDEXING §12.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Read directly from the syntax (containment, a declaration).
    ObservedSyntax,
    /// Resolved through module paths and imports to one declaration.
    InferredStatic,
    /// Several declarations fit.
    CandidateAmbiguous,
    /// Couldn't be resolved.
    Unknown,
}

/// How sure the edge is (§12.6). Separate from a finding's confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GraphConfidence {
    Unknown,
    Low,
    Medium,
    High,
    Exact,
}

/// One relationship, with its evidence (§12.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub kind: EdgeKind,
    pub from: Endpoint,
    pub to: Target,
    /// The file and span where the relationship is written.
    pub file: usize,
    pub span: SourceSpan,
    pub resolution: Resolution,
    pub confidence: GraphConfidence,
    /// Why it isn't exact, as reason codes (`external_crate`, `method_receiver_unknown`).
    pub limitations: Vec<gitcko_domain::Reason>,
}

/// The whole graph for one revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    pub crates: Vec<CrateUnit>,
    pub files: Vec<FileNode>,
    pub symbols: Vec<GraphSymbol>,
    pub edges: Vec<Edge>,
    /// How complete each kind of edge is across the revision: a query over an edge kind
    /// that isn't `complete` can't treat "none found" as "none exist" (IX-024).
    pub coverage: BTreeMap<EdgeKind, Coverage>,
}

/// Most candidates kept for one ambiguous edge; more are cut and the edge says so.
const MAX_CANDIDATES: usize = 32;

/// Builds the graph from parsed files and manifests. Deterministic: the same input gives
/// the same graph, in the same order.
pub fn build(input: &GraphInput<'_>) -> Graph {
    let crates::Layout { crates, files } = crates::layout(input);
    let by_path: HashMap<&str, &Extraction> = input
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.extraction))
        .collect();
    let extractions: Vec<&Extraction> = files.iter().map(|f| by_path[f.path.as_str()]).collect();

    let mut symbols = Vec::new();
    for (fi, f) in files.iter().enumerate() {
        let x = extractions[fi];
        let crate_name = f.crate_unit.map_or("", |u| crates[u].name.as_str());
        for (local, s) in x.symbols.iter().enumerate() {
            let mut parts: Vec<String> = Vec::new();
            if !crate_name.is_empty() {
                parts.push(crate_name.to_string());
            }
            parts.extend(f.module.iter().cloned());
            parts.extend(x.scope_path(s.scope));
            parts.push(s.name.clone());
            let qualified_name = parts.join("::");
            symbols.push(GraphSymbol {
                key: symbol_key(
                    crate_name,
                    &f.module,
                    s.kind,
                    &qualified_name,
                    s.discriminator.as_deref(),
                ),
                stability: if f.module_inferred {
                    KeyStability::Inferred
                } else {
                    KeyStability::Stable
                },
                file: fi,
                local,
                kind: s.kind,
                name: s.name.clone(),
                qualified_name,
                discriminator: s.discriminator.clone(),
                span: s.span,
                in_test: s.in_test,
            });
        }
    }
    let index: HashMap<(usize, usize), usize> = symbols
        .iter()
        .enumerate()
        .map(|(i, s)| ((s.file, s.local), i))
        .collect();
    let libraries = crates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.target == "lib")
        .map(|(i, c)| (c.name.clone(), i))
        .collect();
    let mut resolver = resolve::Resolver::new(&files, &symbols, &extractions, libraries);
    for (fi, x) in extractions.iter().enumerate() {
        for imp in &x.implementations {
            let scope = x.scope_path(imp.scope);
            let types = type_symbols(&resolver, &symbols, fi, &scope, &imp.type_path);
            let mut member_scope = scope;
            member_scope.push(imp.type_path.join("::"));
            resolver.add_impl_members(&types, fi, member_scope);
        }
    }

    let mut edges = Vec::new();
    let file_start = SourceSpan::new(
        0,
        0,
        Position { row: 0, column: 0 },
        Position { row: 0, column: 0 },
    )
    .expect("an empty span at the start is valid");
    for (fi, f) in files.iter().enumerate() {
        let x = extractions[fi];
        let observed = |kind, from, to, span| Edge {
            kind,
            from,
            to: Target::Resolved(to),
            file: fi,
            span,
            resolution: Resolution::ObservedSyntax,
            confidence: GraphConfidence::Exact,
            limitations: Vec::new(),
        };
        if let Some(unit) = f.crate_unit {
            edges.push(observed(
                EdgeKind::Contains,
                Endpoint::Module(unit, f.module.clone()),
                Endpoint::File(fi),
                file_start,
            ));
        }
        for (local, s) in x.symbols.iter().enumerate() {
            let Some(&gi) = index.get(&(fi, local)) else {
                continue;
            };
            let parent = s
                .parent
                .and_then(|p| index.get(&(fi, p)))
                .map_or(Endpoint::File(fi), |&p| Endpoint::Symbol(p));
            edges.push(observed(
                EdgeKind::Declares,
                Endpoint::File(fi),
                Endpoint::Symbol(gi),
                s.span,
            ));
            edges.push(observed(
                EdgeKind::Contains,
                parent,
                Endpoint::Symbol(gi),
                s.span,
            ));
        }
        for imp in &x.imports {
            let res = resolver.resolve(fi, &x.scope_path(imp.scope), &imp.path);
            edges.push(resolved_edge(
                EdgeKind::Imports,
                Endpoint::File(fi),
                res,
                &imp.path,
                fi,
                imp.span,
            ));
        }
        for r in &x.references {
            let from = r
                .enclosing
                .and_then(|e| index.get(&(fi, e)))
                .map_or(Endpoint::File(fi), |&s| Endpoint::Symbol(s));
            let scope = x.scope_path(r.scope);
            let edge = match r.kind {
                RefKind::MethodCall => method_edge(&resolver, from, &r.path, fi, r.span),
                RefKind::Call => resolved_edge(
                    EdgeKind::Calls,
                    from,
                    resolver.resolve(fi, &scope, &r.path),
                    &r.path,
                    fi,
                    r.span,
                ),
                RefKind::MacroArgCall => {
                    let mut e = resolved_edge(
                        EdgeKind::Calls,
                        from,
                        resolver.resolve(fi, &scope, &r.path),
                        &r.path,
                        fi,
                        r.span,
                    );
                    guess(&mut e, "macro_argument_guess");
                    e
                }
                RefKind::MacroCall | RefKind::Type | RefKind::Path => resolved_edge(
                    EdgeKind::References,
                    from,
                    resolver.resolve(fi, &scope, &r.path),
                    &r.path,
                    fi,
                    r.span,
                ),
            };
            edges.push(edge);
        }
        for imp in &x.implementations {
            let Some(trait_path) = &imp.trait_path else {
                continue;
            };
            let scope = x.scope_path(imp.scope);
            let types = type_symbols(&resolver, &symbols, fi, &scope, &imp.type_path);
            let froms: Vec<Endpoint> = if types.is_empty() {
                vec![Endpoint::File(fi)]
            } else {
                types.iter().map(|&t| Endpoint::Symbol(t)).collect()
            };
            for from in froms {
                let mut e = resolved_edge(
                    EdgeKind::Implements,
                    from.clone(),
                    resolver.resolve(fi, &scope, trait_path),
                    trait_path,
                    fi,
                    imp.span,
                );
                if from == Endpoint::File(fi) {
                    e.limitations.push(reason("impl_type_unresolved"));
                }
                edges.push(e);
            }
        }
        for ext in &x.extensions {
            let Some(&from) = index.get(&(fi, ext.symbol)) else {
                continue;
            };
            let scope = x.scope_path(x.symbols[ext.symbol].scope);
            let res = resolver.resolve(fi, &scope, &ext.target);
            edges.push(resolved_edge(
                EdgeKind::Extends,
                Endpoint::Symbol(from),
                res,
                &ext.target,
                fi,
                ext.span,
            ));
        }
    }
    // A test that calls a non-test function is a test of it: a heuristic, and labelled one.
    let tests: Vec<Edge> = edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .filter_map(|e| {
            let (Endpoint::Symbol(from), Target::Resolved(Endpoint::Symbol(to))) = (&e.from, &e.to)
            else {
                return None;
            };
            (symbols[*from].in_test && !symbols[*to].in_test).then(|| Edge {
                kind: EdgeKind::Tests,
                resolution: Resolution::InferredStatic,
                confidence: e.confidence.min(GraphConfidence::Medium),
                limitations: vec![reason("test_heuristic")],
                ..e.clone()
            })
        })
        .collect();
    edges.extend(tests);

    let coverage = coverage(&edges, &extractions);
    Graph {
        crates,
        files,
        symbols,
        edges,
        coverage,
    }
}

/// The symbols an `impl` block's type path names: types, not functions or modules.
fn type_symbols(
    resolver: &resolve::Resolver<'_>,
    symbols: &[GraphSymbol],
    file: usize,
    scope: &[String],
    path: &[String],
) -> Vec<usize> {
    match resolver.resolve(file, scope, path) {
        resolve::Resolved::Found(found) => found
            .into_iter()
            .filter_map(|e| match e {
                Endpoint::Symbol(s)
                    if matches!(
                        symbols[s].kind,
                        SymbolKind::Struct
                            | SymbolKind::Enum
                            | SymbolKind::TypeAlias
                            | SymbolKind::Trait
                    ) =>
                {
                    Some(s)
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// An edge from a resolution: one target is settled, several stay candidates, none is
/// unresolved with the reason.
fn resolved_edge(
    kind: EdgeKind,
    from: Endpoint,
    res: resolve::Resolved,
    path: &[String],
    file: usize,
    span: SourceSpan,
) -> Edge {
    let (to, resolution, confidence, limitations) = match res {
        resolve::Resolved::Found(mut found) if found.len() == 1 => (
            Target::Resolved(found.remove(0)),
            Resolution::InferredStatic,
            GraphConfidence::High,
            Vec::new(),
        ),
        resolve::Resolved::Found(found) => {
            let mut limitations = vec![reason("ambiguous_name")];
            if found.len() > MAX_CANDIDATES {
                limitations.push(reason("candidates_truncated"));
            }
            (
                Target::Candidates(found.into_iter().take(MAX_CANDIDATES).collect()),
                Resolution::CandidateAmbiguous,
                GraphConfidence::Low,
                limitations,
            )
        }
        resolve::Resolved::External => (
            Target::Unresolved(path.to_vec()),
            Resolution::Unknown,
            GraphConfidence::Unknown,
            vec![reason("external_crate")],
        ),
        resolve::Resolved::Missing => (
            Target::Unresolved(path.to_vec()),
            Resolution::Unknown,
            GraphConfidence::Unknown,
            vec![reason("unresolved_name")],
        ),
    };
    Edge {
        kind,
        from,
        to,
        file,
        span,
        resolution,
        confidence,
        limitations,
    }
}

/// `x.name()`: without types, any method called `name` could be the one.
fn method_edge(
    resolver: &resolve::Resolver<'_>,
    from: Endpoint,
    path: &[String],
    file: usize,
    span: SourceSpan,
) -> Edge {
    let found: Vec<Endpoint> = path
        .last()
        .and_then(|name| resolver.methods.get(name))
        .map(|m| m.iter().map(|&s| Endpoint::Symbol(s)).collect())
        .unwrap_or_default();
    let mut limitations = vec![reason("method_receiver_unknown")];
    let to = match found.len() {
        0 => Target::Unresolved(path.to_vec()),
        1 => Target::Resolved(found[0].clone()),
        n => {
            if n > MAX_CANDIDATES {
                limitations.push(reason("candidates_truncated"));
            }
            Target::Candidates(found.into_iter().take(MAX_CANDIDATES).collect())
        }
    };
    let unresolved = matches!(to, Target::Unresolved(_));
    Edge {
        kind: EdgeKind::Calls,
        from,
        to,
        file,
        span,
        resolution: if unresolved {
            Resolution::Unknown
        } else {
            Resolution::CandidateAmbiguous
        },
        confidence: if unresolved {
            GraphConfidence::Unknown
        } else {
            GraphConfidence::Low
        },
        limitations,
    }
}

/// Downgrades an edge that came from a guess (a call seen in macro arguments).
fn guess(edge: &mut Edge, why: &str) {
    if edge.resolution != Resolution::Unknown {
        edge.resolution = Resolution::CandidateAmbiguous;
        edge.confidence = GraphConfidence::Low;
    }
    edge.limitations.push(reason(why));
}

/// Coverage per edge kind (IX-024). External targets don't count against it: they're
/// outside the repository, and say so on their own edges. Uncertainty inside the
/// repository does: an ambiguous or unresolved name, or a file only partly read.
fn coverage(edges: &[Edge], extractions: &[&Extraction]) -> BTreeMap<EdgeKind, Coverage> {
    let partial = |code: &str, omitted: u64| Coverage::Partial {
        reason: reason(code),
        omitted: Some(omitted),
    };
    let partial_files = extractions
        .iter()
        .filter(|x| !x.coverage.is_complete())
        .count() as u64;
    let partial_references = extractions
        .iter()
        .filter(|x| !x.reference_coverage.is_complete())
        .count() as u64;
    let uncertain = |kind: EdgeKind| {
        edges
            .iter()
            .filter(|e| e.kind == kind)
            .filter(|e| {
                e.resolution == Resolution::CandidateAmbiguous
                    || e.limitations
                        .iter()
                        .any(|r| r.as_str() == "unresolved_name")
            })
            .count() as u64
    };
    let mut map = BTreeMap::new();
    for kind in [EdgeKind::Contains, EdgeKind::Declares] {
        map.insert(
            kind,
            if partial_files > 0 {
                partial("file_partial", partial_files)
            } else {
                Coverage::Complete {}
            },
        );
    }
    for kind in [EdgeKind::Imports, EdgeKind::Implements, EdgeKind::Extends] {
        let n = uncertain(kind);
        map.insert(
            kind,
            if n > 0 {
                partial("unresolved_or_ambiguous", n)
            } else {
                Coverage::Complete {}
            },
        );
    }
    for kind in [EdgeKind::References, EdgeKind::Calls] {
        let n = uncertain(kind);
        map.insert(
            kind,
            if n > 0 {
                partial("unresolved_or_ambiguous", n)
            } else if partial_references > 0 {
                partial("file_references_partial", partial_references)
            } else {
                Coverage::Complete {}
            },
        );
    }
    let tests = if map[&EdgeKind::Calls].is_complete() {
        Coverage::Complete {}
    } else {
        Coverage::Partial {
            reason: reason("depends_on_calls"),
            omitted: None,
        }
    };
    map.insert(EdgeKind::Tests, tests);
    map
}

/// The logical identity of a symbol (INDEXING §11.3): no line numbers in it.
fn symbol_key(
    crate_name: &str,
    module: &[String],
    kind: SymbolKind,
    qualified: &str,
    discriminator: Option<&str>,
) -> SymbolKey {
    use sha2::Digest;
    let mut hash = sha2::Sha256::new();
    for part in [
        "rust",
        crate_name,
        &module.join("::"),
        &format!("{kind:?}"),
        qualified,
        discriminator.unwrap_or(""),
    ] {
        hash.update(part.as_bytes());
        // A separator that can't occur inside a part, so ("a", "bc") and ("ab", "c") differ.
        hash.update([0]);
    }
    let digest = hash.finalize();
    SymbolKey(format!(
        "s_{}",
        digest[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}

fn reason(code: &str) -> gitcko_domain::Reason {
    gitcko_domain::Reason::new(code).expect("reason codes in this crate are valid")
}
