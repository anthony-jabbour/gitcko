// SPDX-License-Identifier: AGPL-3.0-or-later

//! Chunking (INDEXING §14–15, §38.5).

use gitcko_domain::RepoPath;
use gitcko_index::{
    Chunk, ChunkKind, ChunkPolicy, ChunkSymbol, FileContext, chunk_code, chunk_text,
};
use gitcko_syntax::{FileRole, Language, ParseLimits, SymbolKind, adapter};
use proptest::prelude::*;

fn path(p: &str) -> RepoPath {
    RepoPath::parse(p).unwrap()
}

/// Chunks Rust source with symbols straight from the adapter (graph index = local index).
fn rust_chunks(source: &str, policy: &ChunkPolicy) -> Vec<Chunk> {
    let x = adapter(Language::Rust)
        .unwrap()
        .extract(source, &ParseLimits::default());
    let names: Vec<String> = x
        .symbols
        .iter()
        .map(|s| {
            let mut q = x.scope_path(s.scope);
            q.push(s.name.clone());
            q.join("::")
        })
        .collect();
    let symbols: Vec<ChunkSymbol<'_>> = x
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| !matches!(s.kind, SymbolKind::Field) && s.native_kind != "enum_variant")
        .map(|(i, s)| ChunkSymbol {
            graph_index: i,
            kind: s.kind,
            span: s.span,
            qualified_name: &names[i],
            signature: s.signature.as_deref(),
            parent: None,
        })
        .collect();
    let p = path("src/lib.rs");
    let file = FileContext {
        path: &p,
        language: Some(Language::Rust),
        role: FileRole::Source,
    };
    chunk_code(&file, source, &symbols, policy).0
}

fn kinds(chunks: &[Chunk]) -> Vec<&'static str> {
    chunks.iter().map(|c| c.kind.as_str()).collect()
}

const SMALL: &str = "use std::fmt;\nuse crate::x::Y;\n\n/// Adds.\npub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n\nfn sub(a: u32, b: u32) -> u32 {\n    a - b\n}\n\nimpl fmt::Display for Y {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {\n        Ok(())\n    }\n}\n";

#[test]
fn small_declarations_stay_whole_and_apart_with_the_preamble_kept() {
    let chunks = rust_chunks(SMALL, &ChunkPolicy::default());
    assert_eq!(
        kinds(&chunks),
        [
            "file_preamble",
            "symbol",
            "symbol",
            "module_context",
            "symbol",
            "module_context"
        ]
    );
    assert!(
        chunks[0].text.contains("use std::fmt;") && chunks[0].text.contains("use crate::x::Y;")
    );
    // The doc comment right above a declaration travels with it.
    assert!(chunks[1].text.starts_with("/// Adds.\npub fn add") && chunks[1].text.ends_with('}'));
    // `impl … {` and its closing brace belong to no method, so they're context.
    assert!(chunks[3].text.contains("impl fmt::Display for Y"));
}

#[test]
fn headers_are_deterministic_metadata() {
    let chunks = rust_chunks(SMALL, &ChunkPolicy::default());
    insta::assert_snapshot!(
        chunks
            .iter()
            .map(|c| format!(
                "--- {} L{}-{}\n{}",
                c.kind.as_str(),
                c.span.lines().0,
                c.span.lines().1,
                c.header
            ))
            .collect::<String>()
    );
}

#[test]
fn an_oversized_function_splits_into_ordered_overlapping_segments() {
    let body: String = (0..300)
        .map(|i| format!("    let v{i} = compute({i});\n"))
        .collect();
    let source = format!("fn big() {{\n{body}}}\n");
    let policy = ChunkPolicy::default();
    let chunks = rust_chunks(&source, &policy);
    assert!(
        chunks.len() >= 2 && chunks.iter().all(|c| c.kind == ChunkKind::SymbolSegment),
        "{:?}",
        kinds(&chunks)
    );
    let count = chunks.len();
    for (i, c) in chunks.iter().enumerate() {
        let seg = c.segment.unwrap();
        assert_eq!((seg.index, seg.count), (i + 1, count));
        assert!(c.text.len() <= policy.soft_max_bytes);
        assert!(c.header.contains(&format!("segment: {}/{count}", i + 1)));
    }
    for pair in chunks.windows(2) {
        // Each segment starts before the previous one ends (the overlap), at a line start.
        assert!(pair[1].span.start_byte() < pair[0].span.end_byte());
        assert_eq!(pair[1].span.start().column, 0);
    }
    assert_eq!(rust_chunks(&source, &policy), chunks, "deterministic");
}

#[test]
fn an_oversized_container_gives_way_to_its_members() {
    let methods: String = (0..120)
        .map(|i| format!("    fn m{i}(&self) -> u32 {{\n        {i} + {i} * 2 + {i} * 3\n    }}\n"))
        .collect();
    let source = format!("trait Big {{\n{methods}}}\n");
    let chunks = rust_chunks(&source, &ChunkPolicy::default());
    let symbols = chunks
        .iter()
        .filter(|c| c.kind == ChunkKind::Symbol)
        .count();
    assert_eq!(symbols, 120);
    assert!(
        chunks
            .iter()
            .any(|c| c.kind == ChunkKind::FilePreamble || c.kind == ChunkKind::ModuleContext)
    );
}

#[test]
fn text_files_chunk_by_their_own_structure() {
    let p = path("docs/guide.md");
    let doc = FileContext {
        path: &p,
        language: Some(Language::Markdown),
        role: FileRole::Documentation,
    };
    let sections: String = (0..4)
        .map(|i| format!("# Part {i}\n{}\n", "words ".repeat(800)))
        .collect();
    let (chunks, _) = chunk_text(&doc, &sections, &ChunkPolicy::default());
    assert_eq!(chunks.len(), 4);
    assert!(
        chunks
            .iter()
            .all(|c| c.kind == ChunkKind::DocumentationSection && c.text.starts_with("# Part"))
    );
    let p = path("config/app.yaml");
    let cfg = FileContext {
        path: &p,
        language: Some(Language::Yaml),
        role: FileRole::Configuration,
    };
    let (chunks, _) = chunk_text(&cfg, "a: 1\n\nb: 2\n\nc: 3\n", &ChunkPolicy::default());
    assert_eq!(
        kinds(&chunks),
        ["configuration_block"],
        "small blocks merge"
    );
}

#[test]
fn the_per_file_chunk_limit_cuts_with_a_reason() {
    let source: String = (0..10).map(|i| format!("fn f{i}() {{}}\n")).collect();
    let policy = ChunkPolicy {
        max_chunks_per_file: 3,
        ..ChunkPolicy::default()
    };
    let x = adapter(Language::Rust)
        .unwrap()
        .extract(&source, &ParseLimits::default());
    let names: Vec<String> = x.symbols.iter().map(|s| s.name.clone()).collect();
    let symbols: Vec<ChunkSymbol<'_>> = x
        .symbols
        .iter()
        .enumerate()
        .map(|(i, s)| ChunkSymbol {
            graph_index: i,
            kind: s.kind,
            span: s.span,
            qualified_name: &names[i],
            signature: None,
            parent: None,
        })
        .collect();
    let p = path("a.rs");
    let file = FileContext {
        path: &p,
        language: Some(Language::Rust),
        role: FileRole::Source,
    };
    let (chunks, coverage) = chunk_code(&file, &source, &symbols, &policy);
    assert_eq!(chunks.len(), 3);
    assert!(
        matches!(
            coverage,
            gitcko_domain::Coverage::Partial {
                omitted: Some(7),
                ..
            }
        ),
        "{coverage:?}"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Every non-whitespace byte lands in a chunk, every chunk's text is exactly its span,
    /// and its hash matches (INDEXING §38.5).
    #[test]
    fn chunks_cover_the_source_and_match_it(
        fns in proptest::collection::vec((0usize..3, 0usize..120), 1..12),
        noise in "[ a-z;\n/]{0,30}",
        small in proptest::bool::ANY,
    ) {
        let mut source = format!("use a::b;\n{noise}\n");
        for (i, (kind, lines)) in fns.iter().enumerate() {
            let body: String = (0..*lines).map(|l| format!("    x{l}();\n")).collect();
            source += &match kind {
                0 => format!("fn f{i}() {{\n{body}}}\n"),
                1 => format!("// note {i}\nstruct S{i} {{ a: u8 }}\n"),
                _ => format!("impl S{i} {{\n    fn m(&self) {{\n{body}    }}\n}}\n"),
            };
        }
        let policy = if small {
            ChunkPolicy { target_bytes: 120, soft_max_bytes: 160, hard_max_bytes: 240, overlap_bytes: 30, ..ChunkPolicy::default() }
        } else {
            ChunkPolicy::default()
        };
        let chunks = rust_chunks(&source, &policy);
        let mut covered = vec![false; source.len()];
        for c in &chunks {
            prop_assert_eq!(&source[c.span.start_byte()..c.span.end_byte()], c.text.as_str());
            prop_assert_eq!(c.content_hash, gitcko_domain::ContentHash::of(c.text.as_bytes()));
            covered[c.span.start_byte()..c.span.end_byte()].fill(true);
        }
        for (i, b) in source.bytes().enumerate() {
            prop_assert!(covered[i] || b.is_ascii_whitespace(), "byte {} ({:?}) uncovered", i, b as char);
        }
        // Only segments of one split declaration overlap; nothing else does.
        for pair in chunks.windows(2) {
            let same_split = pair[0].kind == ChunkKind::SymbolSegment
                && pair[1].kind == ChunkKind::SymbolSegment
                && pair[0].symbol == pair[1].symbol;
            prop_assert!(same_split || pair[0].span.end_byte() <= pair[1].span.start_byte(), "{:?} overlaps {:?}", pair[0].span, pair[1].span);
        }
    }
}
