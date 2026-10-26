// SPDX-License-Identifier: AGPL-3.0-or-later

//! Building and validating a revision (INDEXING §26).

use std::collections::BTreeMap;

use gitcko_domain::RepoPath;
use gitcko_git::Repo;
use gitcko_index::{IndexError, IndexLimits, Revision, build, validate};
use gitcko_search::{Query, QueryMode};
use gitcko_test_support::{empty_repo, literal_commit};

const FILES: &[(&[u8], &str, &[u8])] = &[
    (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\nversion = \"0.1.0\"\n"),
    (b"src/lib.rs", "100644", b"//! Demo crate.\nuse crate::util::helper;\n\nmod util;\n\n/// Totals things.\npub fn total(xs: &[u32]) -> u32 {\n    helper(xs)\n}\n"),
    (b"src/util.rs", "100644", b"pub fn helper(xs: &[u32]) -> u32 {\n    xs.iter().sum()\n}\n"),
    (b"README.md", "100644", b"# Demo\n\nA demo.\n\n## Use\n\nCall total.\n"),
    (b"config/app.yaml", "100644", b"name: demo\nlevel: 3\n"),
    (b"logo.png", "100644", b"\x89PNG\r\n\x1a\n\0\0\0"),
];

fn built(files: &[(&[u8], &str, &[u8])]) -> (Revision, gitcko_test_support::TempDir) {
    let dir = empty_repo("build");
    let commit = literal_commit(dir.path(), files);
    let repo = Repo::open(dir.path()).unwrap();
    let rev = build(
        &repo,
        repo.resolve(&commit).unwrap(),
        &IndexLimits::default(),
    )
    .unwrap();
    (rev, dir)
}

#[test]
fn a_revision_holds_every_layer_and_says_how_complete_each_is() {
    let (rev, _dir) = built(FILES);
    let mut out = format!("versions: {}\n", rev.versions.label());
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &rev.chunks {
        *kinds.entry(c.kind.as_str()).or_default() += 1;
    }
    out += &format!(
        "parsed files: {}\nsymbols: {}\nchunks: {kinds:?}\nlexical docs: {}\n",
        rev.parsed.len(),
        rev.graph.symbols.len(),
        rev.lexical.len()
    );
    for c in &rev.chunks {
        out += &format!(
            "chunk {} {} L{}-{}\n",
            c.kind.as_str(),
            c.path.as_str(),
            c.span.lines().0,
            c.span.lines().1
        );
    }
    let cov = &rev.coverage;
    out += &format!(
        "coverage discovery={:?} parsing={:?} chunking={:?} lexical={:?} semantic={:?}\n",
        cov.discovery, cov.parsing, cov.chunking, cov.lexical, cov.semantic
    );
    insta::assert_snapshot!(out);
}

#[test]
fn the_lexical_index_finds_chunks_by_name_and_path() {
    let (rev, _dir) = built(FILES);
    let hits = rev
        .lexical
        .search(&Query::new(QueryMode::Identifier, "helper"), 10, 0)
        .unwrap();
    let paths: Vec<&str> = hits
        .hits
        .iter()
        .map(|h| rev.chunks[h.id as usize].path.as_str())
        .collect();
    assert_eq!(paths[0], "src/util.rs", "{paths:?}");
    let hits = rev
        .lexical
        .search(&Query::new(QueryMode::Tokens, "demo"), 10, 0)
        .unwrap();
    assert!(
        hits.hits
            .iter()
            .any(|h| rev.chunks[h.id as usize].path.as_str() == "README.md")
    );
}

type Files<'a> = [(&'a [u8], &'a str, &'a [u8])];

fn source_of<'a>(files: &'a Files<'a>) -> impl Fn(&RepoPath) -> Option<String> + 'a {
    move |p: &RepoPath| {
        files
            .iter()
            .find(|(path, _, _)| *path == p.as_str().as_bytes())
            .map(|(_, _, c)| String::from_utf8(c.to_vec()).unwrap())
    }
}

#[test]
fn validation_rejects_a_chunk_that_no_longer_matches_its_source() {
    let (mut rev, _dir) = built(FILES);
    assert_eq!(validate(&rev, source_of(FILES)), Ok(()));
    rev.chunks[1].text.push('!');
    assert_eq!(
        validate(&rev, source_of(FILES)),
        Err(IndexError::Invalid {
            check: "chunk_hash"
        })
    );
}

#[test]
fn validation_rejects_a_chunk_outside_its_file() {
    // The same revision checked against a source where `src/util.rs` is now one byte.
    let (rev, _dir) = built(FILES);
    let short: Vec<(&[u8], &str, &[u8])> = FILES
        .iter()
        .map(|(p, m, c)| (*p, *m, if *p == b"src/util.rs" { &b"x"[..] } else { *c }))
        .collect();
    assert_eq!(
        validate(&rev, source_of(&short)),
        Err(IndexError::Invalid {
            check: "chunk_bounds"
        })
    );
}

#[test]
fn validation_rejects_a_dangling_graph_edge() {
    let (mut rev, _dir) = built(FILES);
    let edge = rev.graph.edges[0].clone();
    rev.graph.edges.push(gitcko_graph::Edge {
        from: gitcko_graph::Endpoint::Symbol(10_000),
        ..edge
    });
    assert_eq!(
        validate(&rev, source_of(FILES)),
        Err(IndexError::Invalid {
            check: "graph_endpoint"
        })
    );
}

#[test]
fn validation_rejects_a_lexical_index_that_doesnt_match_the_chunks() {
    let (mut rev, _dir) = built(FILES);
    rev.chunks.pop();
    assert_eq!(
        validate(&rev, source_of(FILES)),
        Err(IndexError::Invalid {
            check: "lexical_count"
        })
    );
}

#[test]
fn canary_a_failed_check_names_the_check_only() {
    let err = IndexError::Invalid {
        check: "chunk_hash",
    };
    for canary in gitcko_redact::canaries() {
        assert!(!format!("{err} {err:?}").contains(&canary));
    }
}
