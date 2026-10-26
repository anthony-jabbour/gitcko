// SPDX-License-Identifier: AGPL-3.0-or-later

//! Query APIs and context retrieval (INDEXING §20–22, §38.6).

use gitcko_domain::RepoPath;
use gitcko_git::Repo;
use gitcko_graph::Endpoint;
use gitcko_index::{
    CodeQuery, CodeSearchMode, IndexError, IndexLimits, RetrievalBudget, Revision, Source,
    TestEvidence, build,
};
use gitcko_search::QueryMode;
use gitcko_test_support::{TempDir, empty_repo, literal_commit};

const FILES: &[(&[u8], &str, &[u8])] = &[
    (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\n"),
    (
        b"src/lib.rs",
        "100644",
        b"mod util;\nmod other;\n\npub fn total(xs: &[u32]) -> u32 {\n    util::helper(xs) + other::helper()\n}\n\npub fn unused() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn total_adds() {\n        super::total(&[]);\n    }\n}\n",
    ),
    (b"src/util.rs", "100644", b"pub fn helper(xs: &[u32]) -> u32 {\n    let mut sum = 0;\n    for x in xs {\n        sum += x;\n    }\n    sum\n}\n"),
    (b"src/other.rs", "100644", b"pub fn helper() -> u32 {\n    7\n}\n"),
];

fn built(files: &[(&[u8], &str, &[u8])]) -> (Revision, Repo, TempDir) {
    let dir = empty_repo("query");
    let commit = literal_commit(dir.path(), files);
    let repo = Repo::open(dir.path()).unwrap();
    let rev = build(
        &repo,
        repo.resolve(&commit).unwrap(),
        &IndexLimits::default(),
    )
    .unwrap();
    (rev, repo, dir)
}

fn sym(rev: &Revision, qualified: &str) -> usize {
    rev.graph
        .symbols
        .iter()
        .position(|s| s.qualified_name == qualified)
        .unwrap_or_else(|| panic!("{qualified}"))
}

fn names(rev: &Revision, edges: &[usize], from: bool) -> Vec<String> {
    edges
        .iter()
        .map(|&e| {
            let edge = &rev.graph.edges[e];
            let end = if from {
                Some(&edge.from)
            } else if let gitcko_graph::Target::Resolved(t) = &edge.to {
                Some(t)
            } else {
                None
            };
            match end {
                Some(Endpoint::Symbol(s)) => rev.graph.symbols[*s].qualified_name.clone(),
                Some(other) => format!("{other:?}"),
                None => "?".into(),
            }
        })
        .collect()
}

#[test]
fn find_symbol_returns_every_same_named_declaration() {
    let (rev, _repo, _dir) = built(FILES);
    let m = rev.find_symbol("helper", None, 10);
    let found: Vec<&str> = m
        .symbols
        .iter()
        .map(|&s| rev.graph.symbols[s].qualified_name.as_str())
        .collect();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.contains(&"demo::util::helper") && found.contains(&"demo::other::helper"));
    // An exact qualified name comes first.
    let m = rev.find_symbol("demo::util::helper", None, 10);
    assert_eq!(
        rev.graph.symbols[m.symbols[0]].qualified_name,
        "demo::util::helper"
    );
    let only = rev.find_symbol(
        "helper",
        Some(&RepoPath::parse("src/other.rs").unwrap()),
        10,
    );
    assert_eq!(only.symbols.len(), 1);
}

#[test]
fn callers_and_callees_keep_resolved_and_unresolved_apart() {
    let (rev, _repo, _dir) = built(FILES);
    let callers = rev.find_callers(sym(&rev, "demo::util::helper"), 10);
    assert_eq!(names(&rev, &callers.resolved, true), ["demo::total"]);
    let callees = rev.find_callees(sym(&rev, "demo::total"), 10);
    assert!(callees.coverage.is_complete(), "{:?}", callees.coverage);
    let mut out = names(&rev, &callees.resolved, false);
    out.sort();
    assert_eq!(out, ["demo::other::helper", "demo::util::helper"]);
}

#[test]
fn zero_callers_under_partial_coverage_is_not_zero_callers() {
    // INDEXING §27.6 / IX-024: the same empty answer, two different meanings.
    let (rev, _repo, _dir) = built(FILES);
    let complete = rev.find_callers(sym(&rev, "demo::unused"), 10);
    assert!(complete.resolved.is_empty() && complete.candidates.is_empty());
    assert!(complete.coverage.is_complete(), "{:?}", complete.coverage);

    // One method call somewhere makes calls coverage partial: `[]` no longer proves none.
    let mut with_method: Vec<(&[u8], &str, &[u8])> = FILES.to_vec();
    with_method.push((
        b"src/extra.rs",
        "100644",
        b"pub fn x(v: &str) -> usize { v.len() }\n",
    ));
    let lib = b"mod util;\nmod other;\nmod extra;\n\npub fn total(xs: &[u32]) -> u32 {\n    util::helper(xs) + other::helper()\n}\n\npub fn unused() {}\n";
    with_method[1] = (b"src/lib.rs", "100644", lib);
    let (rev, _repo, _dir) = built(&with_method);
    let partial = rev.find_callers(sym(&rev, "demo::unused"), 10);
    assert!(partial.resolved.is_empty() && partial.candidates.is_empty());
    assert!(!partial.coverage.is_complete(), "{:?}", partial.coverage);
    let callees = rev.find_callees(sym(&rev, "demo::unused"), 10);
    assert!(callees.resolved.is_empty() && !callees.coverage.is_complete());
}

#[test]
fn an_exact_qualified_name_outranks_a_longer_path_ending_the_same_way() {
    let (rev, _repo, _dir) = built(&[
        (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\n"),
        (
            b"src/lib.rs",
            "100644",
            b"pub mod a {\n    pub mod demo {\n        pub mod util {\n            pub fn helper() {}\n        }\n    }\n}\npub mod util {\n    pub fn helper() {}\n}\n",
        ),
    ]);
    let m = rev.find_symbol("demo::util::helper", None, 10);
    let found: Vec<&str> = m
        .symbols
        .iter()
        .map(|&s| rev.graph.symbols[s].qualified_name.as_str())
        .collect();
    assert_eq!(found, ["demo::util::helper", "demo::a::demo::util::helper"]);
}

#[test]
fn importers_and_tests_are_found() {
    let (rev, _repo, _dir) = built(FILES);
    let tests = rev.find_tests(sym(&rev, "demo::total"), 10);
    assert_eq!(tests.tests.len(), 1, "{:?}", tests.tests);
    assert_eq!(rev.graph.symbols[tests.tests[0].0].name, "total_adds");
    assert_eq!(tests.tests[0].1, TestEvidence::Calls);
    let util = rev
        .graph
        .crates
        .iter()
        .position(|c| c.name == "demo")
        .unwrap();
    let importers = rev.find_importers(&Endpoint::Module(util, vec!["util".into()]), 10);
    assert!(
        importers.resolved.is_empty(),
        "no `use` of the module in this repo"
    );
}

fn query(text: &str, mode: CodeSearchMode, limit: usize) -> CodeQuery {
    CodeQuery {
        text: text.into(),
        mode,
        language: None,
        role: None,
        path_prefix: None,
        limit,
        offset: 0,
    }
}

#[test]
fn literal_search_finds_exact_text_the_index_would_tokenize_away() {
    let (rev, repo, _dir) = built(FILES);
    let r = rev
        .search_code(
            &repo,
            &query("sum += x;", CodeSearchMode::Literal, 10),
            1 << 20,
        )
        .unwrap();
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].path.as_str(), "src/util.rs");
    assert_eq!(r.hits[0].span.lines(), (4, 4));
    assert!(r.coverage.is_complete() && !r.truncated);
    // A page shorter than the matches says so.
    let r = rev
        .search_code(&repo, &query("helper", CodeSearchMode::Literal, 1), 1 << 20)
        .unwrap();
    assert_eq!((r.hits.len(), r.truncated), (1, true));
    // A scan budget smaller than the files is partial, not complete.
    let r = rev
        .search_code(
            &repo,
            &query("nothing-here", CodeSearchMode::Literal, 10),
            10,
        )
        .unwrap();
    assert!(
        r.hits.is_empty() && !r.coverage.is_complete(),
        "{:?}",
        r.coverage
    );
    assert_eq!(
        rev.search_code(&repo, &query("", CodeSearchMode::Literal, 10), 1 << 20),
        Err(IndexError::EmptyQuery)
    );
}

#[test]
fn lexical_search_reports_totals() {
    let (rev, repo, _dir) = built(FILES);
    let r = rev
        .search_code(
            &repo,
            &query("helper", CodeSearchMode::Lexical(QueryMode::Identifier), 10),
            0,
        )
        .unwrap();
    assert!(r.total.unwrap() >= 2);
    assert!(r.hits.iter().all(|h| h.chunk.is_some()));
}

#[test]
fn overview_is_deterministic_counts() {
    let (rev, _repo, _dir) = built(FILES);
    let o = rev.repo_overview();
    assert_eq!(
        (o.files, o.parsed_files, o.crates.clone()),
        (4, 3, vec!["demo".to_string()])
    );
    assert_eq!(
        o.languages,
        [("rust".to_string(), 3), ("toml".to_string(), 1)]
    );
}

#[test]
fn context_for_a_change_puts_the_changed_code_first_and_says_why() {
    let (rev, _repo, _dir) = built(FILES);
    let diff = "diff --git a/src/util.rs b/src/util.rs\n--- a/src/util.rs\n+++ b/src/util.rs\n@@ -4,1 +4,1 @@\n-        sum += x;\n+        sum += *x;\n";
    let change = gitcko_diff::parse(diff).unwrap();
    let bundle = rev.retrieve(&change, &RetrievalBudget::default());
    let first = &bundle.items[0];
    assert_eq!(first.path.as_str(), "src/util.rs");
    assert!(first.sources.iter().any(|(s, _)| *s == Source::Anchor));
    // The caller comes in through the graph.
    assert!(
        bundle.items.iter().any(|i| i.text.contains("pub fn total")
            && i.sources.iter().any(|(s, _)| *s == Source::Graph))
    );
    assert!(!bundle.coverage.semantic.is_complete());
    // Two hops out: the test calling `total`.
    assert!(
        bundle
            .items
            .iter()
            .any(|i| i.text.contains("fn total_adds"))
    );
    let near = rev.retrieve(
        &change,
        &RetrievalBudget {
            graph_depth: 1,
            ..RetrievalBudget::default()
        },
    );
    assert!(!near.items.iter().any(|i| i.text.contains("fn total_adds")));
    // Lexical search for the changed names contributes too.
    assert!(
        bundle
            .items
            .iter()
            .any(|i| i.sources.iter().any(|(s, _)| *s == Source::Lexical))
    );
    // The call between two included items is listed with them.
    let calls: Vec<(&str, &str)> = bundle
        .relationships
        .iter()
        .map(|&e| {
            let edge = &rev.graph.edges[e];
            let name = |end: &Endpoint| match end {
                Endpoint::Symbol(s) => rev.graph.symbols[*s].qualified_name.as_str(),
                _ => "?",
            };
            let to = match &edge.to {
                gitcko_graph::Target::Resolved(t) => name(t),
                _ => "?",
            };
            (name(&edge.from), to)
        })
        .collect();
    assert!(
        calls.contains(&("demo::total", "demo::util::helper")),
        "{calls:?}"
    );
    assert!(!bundle.truncated);
    // A tiny budget keeps the anchor and counts what it left out.
    let tight = rev.retrieve(
        &change,
        &RetrievalBudget {
            max_items: 1,
            ..RetrievalBudget::default()
        },
    );
    assert_eq!(tight.items.len(), 1);
    assert_eq!(tight.items[0].path.as_str(), "src/util.rs");
    assert!(tight.truncated && tight.omitted > 0);
    assert!(
        tight.relationships.is_empty(),
        "nothing to relate the one item to"
    );
}

#[test]
fn canary_query_errors_never_echo_the_query() {
    let (rev, repo, _dir) = built(FILES);
    for canary in gitcko_redact::canaries() {
        for mode in [
            CodeSearchMode::Literal,
            CodeSearchMode::Lexical(QueryMode::Tokens),
        ] {
            for text in [canary.clone(), format!("{canary} !!"), String::new()] {
                if let Err(err) = rev.search_code(&repo, &query(&text, mode, 10), 1 << 20) {
                    assert!(!format!("{err} {err:?}").contains(&canary));
                }
            }
        }
    }
    let err = rev
        .search_code(&repo, &query("", CodeSearchMode::Literal, 10), 1 << 20)
        .unwrap_err();
    assert_eq!(err.to_string(), "the query has nothing to search for");
}

#[test]
fn literal_search_honours_language_and_role_filters() {
    let (rev, repo, _dir) = built(FILES);
    let mut q = query("helper", CodeSearchMode::Literal, 10);
    q.language = Some("toml".into());
    let r = rev.search_code(&repo, &q, 1 << 20).unwrap();
    assert!(r.hits.is_empty(), "{:?}", r.hits);
    q.language = Some("rust".into());
    q.role = Some("source".into());
    let r = rev.search_code(&repo, &q, 1 << 20).unwrap();
    assert!(!r.hits.is_empty());
    q.role = Some("test".into());
    let r = rev.search_code(&repo, &q, 1 << 20).unwrap();
    assert!(r.hits.is_empty(), "{:?}", r.hits);
}

fn built_with(files: &[(&[u8], &str, &[u8])], limits: &IndexLimits) -> (Revision, Repo, TempDir) {
    let dir = empty_repo("query");
    let commit = literal_commit(dir.path(), files);
    let repo = Repo::open(dir.path()).unwrap();
    let rev = build(&repo, repo.resolve(&commit).unwrap(), limits).unwrap();
    (rev, repo, dir)
}

fn has(item: &gitcko_index::ContextItem, source: Source) -> bool {
    item.sources.iter().any(|(s, _)| *s == source)
}

#[test]
fn a_declaration_in_a_file_too_big_to_parse_is_not_proven_absent() {
    let big = format!("pub fn hidden() {{}}\n{}", "// pad\n".repeat(40));
    let files: &[(&[u8], &str, &[u8])] = &[
        (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\n"),
        (b"src/lib.rs", "100644", b"mod big;\npub fn shown() {}\n"),
        (b"src/big.rs", "100644", big.as_bytes()),
    ];
    let limits = IndexLimits {
        max_parse_bytes: 100,
        ..IndexLimits::default()
    };
    let (rev, _repo, _dir) = built_with(files, &limits);
    assert!(rev.coverage.parsing.is_complete(), "the premise");
    let m = rev.find_symbol("hidden", None, 10);
    assert!(m.symbols.is_empty());
    assert!(!m.coverage.is_complete(), "{:?}", m.coverage);
    // Scoped to a file that was parsed, the answer is complete.
    let lib = RepoPath::parse("src/lib.rs").unwrap();
    assert!(
        rev.find_symbol("hidden", Some(&lib), 10)
            .coverage
            .is_complete()
    );
}

#[test]
fn literal_search_counts_unavailable_files_in_its_scope() {
    let mut files: Vec<(&[u8], &str, &[u8])> = FILES.to_vec();
    files.push((b"bad\xff.rs", "100644", b"pub fn x() {}\n"));
    let (rev, repo, _dir) = built(&files);
    let r = rev
        .search_code(
            &repo,
            &query("helper", CodeSearchMode::Literal, 10),
            1 << 20,
        )
        .unwrap();
    assert!(!r.coverage.is_complete(), "{:?}", r.coverage);
    // Outside the scope it isn't a gap.
    let mut q = query("helper", CodeSearchMode::Literal, 10);
    q.path_prefix = Some("src/".into());
    let r = rev.search_code(&repo, &q, 1 << 20).unwrap();
    assert!(r.coverage.is_complete(), "{:?}", r.coverage);
}

#[test]
fn a_full_page_does_not_hide_a_scan_limit() {
    let big = "// pad\n".repeat(200);
    let (rev, repo, _dir) = built(&[
        (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\n"),
        (b"src/a.rs", "100644", b"fn needle() {}\n"),
        (b"src/b.rs", "100644", big.as_bytes()),
    ]);
    let mut q = query("needle", CodeSearchMode::Literal, 1);
    q.path_prefix = Some("src/".into());
    let r = rev.search_code(&repo, &q, 100).unwrap();
    assert_eq!(r.hits.len(), 1);
    assert!(!r.coverage.is_complete(), "{:?}", r.coverage);
}

#[test]
fn a_large_lexical_limit_with_few_matches_is_not_truncated() {
    let (rev, repo, _dir) = built(FILES);
    let r = rev
        .search_code(
            &repo,
            &query(
                "helper",
                CodeSearchMode::Lexical(QueryMode::Identifier),
                600,
            ),
            0,
        )
        .unwrap();
    assert!(!r.truncated && r.hits.len() < 500);
}

#[test]
fn same_module_tests_follow_real_module_ownership() {
    let (rev, _repo, _dir) = built(&[
        (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\n"),
        (
            b"src/lib.rs",
            "100644",
            b"pub struct Foo;\nimpl Foo {\n    pub fn render(&self) {}\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn draws() {}\n}\n",
        ),
        (
            b"src/main.rs",
            "100644",
            b"fn main() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn elsewhere() {}\n}\n",
        ),
    ]);
    let render = rev
        .graph
        .symbols
        .iter()
        .position(|s| s.name == "render")
        .unwrap();
    let found: Vec<(&str, TestEvidence)> = rev
        .find_tests(render, 10)
        .tests
        .iter()
        .map(|&(t, why)| (rev.graph.symbols[t].name.as_str(), why))
        .collect();
    assert_eq!(found, [("draws", TestEvidence::SameModule)]);
}

#[test]
fn context_anchors_only_on_changed_lines() {
    let src = b"pub fn a() -> u32 {\n    1\n}\n\npub fn b() -> u32 {\n    3\n}\n";
    let (rev, _repo, _dir) = built(&[
        (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\n"),
        (b"src/lib.rs", "100644", src),
    ]);
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -3,5 +3,5 @@\n }\n \n pub fn b() -> u32 {\n-    2\n+    3\n }\n";
    let change = gitcko_diff::parse(diff).unwrap();
    let bundle = rev.retrieve(&change, &RetrievalBudget::default());
    let anchors: Vec<&str> = bundle
        .items
        .iter()
        .filter(|i| has(i, Source::Anchor))
        .map(|i| i.text.as_str())
        .collect();
    assert_eq!(anchors.len(), 1, "{anchors:?}");
    assert!(anchors[0].contains("pub fn b"));
}

const NESTED: &[u8] = b"pub mod inner {\n    pub fn nested() -> u32 {\n        1\n    }\n}\n\npub fn caller() -> u32 {\n    inner::nested()\n}\n\npub fn changed() {}\n\npub mod callers {\n    pub fn one() {\n        super::changed();\n    }\n    pub fn two() {\n        super::changed();\n    }\n}\n\n/// Mentions inner, nothing more.\npub fn unrelated() {}\n";

fn nested_change(line: u32, old: &str, new: &str) -> gitcko_diff::ChangeSet {
    let diff = format!(
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -{line},1 +{line},1 @@\n-{old}\n+{new}\n"
    );
    gitcko_diff::parse(&diff).unwrap()
}

#[test]
fn a_nested_change_seeds_the_graph_from_the_symbol_it_touches() {
    let (rev, _repo, _dir) = built(&[
        (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\n"),
        (b"src/lib.rs", "100644", NESTED),
    ]);
    let bundle = rev.retrieve(
        &nested_change(3, "        0", "        1"),
        &RetrievalBudget::default(),
    );
    assert!(
        bundle
            .items
            .iter()
            .any(|i| i.text.contains("pub fn caller") && has(i, Source::Graph)),
        "{:?}",
        bundle.items.iter().map(|i| &i.sources).collect::<Vec<_>>()
    );
    // The module itself isn't a changed symbol: its name isn't searched for.
    assert!(
        !bundle
            .items
            .iter()
            .any(|i| i.text.contains("pub fn unrelated")),
        "{:?}",
        bundle.items.iter().map(|i| &i.text).collect::<Vec<_>>()
    );
    // The call into the module's nested function is related.
    let nested = sym(&rev, "demo::inner::nested");
    assert!(bundle.relationships.iter().any(|&e| {
        rev.graph.edges[e].to == gitcko_graph::Target::Resolved(Endpoint::Symbol(nested))
    }));
}

#[test]
fn several_graph_hits_in_one_chunk_count_once() {
    let (rev, _repo, _dir) = built(&[
        (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\n"),
        (b"src/lib.rs", "100644", NESTED),
    ]);
    let bundle = rev.retrieve(
        &nested_change(11, "pub fn changed() { }", "pub fn changed() {}"),
        &RetrievalBudget::default(),
    );
    let callers = bundle
        .items
        .iter()
        .find(|i| i.text.contains("pub mod callers"))
        .expect("the callers' module");
    let graph = callers
        .sources
        .iter()
        .filter(|(s, _)| *s == Source::Graph)
        .count();
    assert_eq!(graph, 1, "{:?}", callers.sources);
}

#[test]
fn context_keeps_the_lexical_search_cutoff() {
    let (rev, _repo, _dir) = built(FILES);
    let diff = "diff --git a/src/util.rs b/src/util.rs\n--- a/src/util.rs\n+++ b/src/util.rs\n@@ -4,1 +4,1 @@\n-        sum += x;\n+        sum += *x;\n";
    let change = gitcko_diff::parse(diff).unwrap();
    let bundle = rev.retrieve(
        &change,
        &RetrievalBudget {
            lexical_hits: 1,
            ..RetrievalBudget::default()
        },
    );
    assert!(bundle.truncated && bundle.omitted > 0, "{}", bundle.omitted);
}

#[test]
fn a_pure_deletion_anchors_where_the_lines_were() {
    let (rev, _repo, _dir) = built(&[
        (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\n"),
        (b"src/lib.rs", "100644", b"pub fn a() {}\n\npub fn b() {}\n"),
    ]);
    // `// gone` was line 3, just above `b`; git writes the new side as "after line 2".
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -3,1 +2,0 @@\n-// gone\n";
    let bundle = rev.retrieve(
        &gitcko_diff::parse(diff).unwrap(),
        &RetrievalBudget::default(),
    );
    let anchors: Vec<&str> = bundle
        .items
        .iter()
        .filter(|i| has(i, Source::Anchor))
        .map(|i| i.text.as_str())
        .collect();
    assert_eq!(anchors.len(), 1, "{anchors:?}");
    assert!(anchors[0].contains("pub fn b"), "{anchors:?}");
    // Deleting the file's last lines leaves no line after them: the one before anchors.
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -4,1 +3,0 @@\n-// tail\n";
    let bundle = rev.retrieve(
        &gitcko_diff::parse(diff).unwrap(),
        &RetrievalBudget::default(),
    );
    assert!(
        bundle
            .items
            .iter()
            .any(|i| has(i, Source::Anchor) && i.text.contains("pub fn b")),
        "{:?}",
        bundle.items.iter().map(|i| &i.text).collect::<Vec<_>>()
    );
}
