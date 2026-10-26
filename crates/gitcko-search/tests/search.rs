// SPDX-License-Identifier: AGPL-3.0-or-later

//! Lexical search over a small synthetic corpus (INDEXING §38.6).

use gitcko_search::{LexicalDoc, LexicalIndex, Query, QueryMode, SearchError, code_tokens};
use gitcko_test_support::TempDir;

fn doc(
    id: u64,
    path: &str,
    role: &str,
    name: Option<&str>,
    qualified: Option<&str>,
    content: &str,
) -> LexicalDoc {
    LexicalDoc {
        id,
        path: path.into(),
        language: path.ends_with(".rs").then(|| "rust".into()),
        role: role.into(),
        kind: "symbol".into(),
        symbol_key: name.map(|n| format!("key-{n}")),
        name: name.map(Into::into),
        qualified_name: qualified.map(Into::into),
        signature: name.map(|n| format!("fn {n}()")),
        content: content.into(),
    }
}

fn corpus() -> Vec<LexicalDoc> {
    vec![
        doc(
            1,
            "src/auth/session.rs",
            "source",
            Some("refreshSession"),
            Some("auth::session::refreshSession"),
            "pub fn refreshSession(token: &Token) -> Session { let total = compute(token); renew(total) }",
        ),
        doc(
            2,
            "src/config.rs",
            "source",
            Some("load_config"),
            Some("config::load_config"),
            "pub fn load_config(path: &Path) -> Config { read_file(path) }",
        ),
        doc(
            3,
            "tests/api.rs",
            "test",
            Some("refresh_works"),
            Some("refresh_works"),
            "fn refresh_works() { let s = auth::session::refreshSession(&t); compute total }",
        ),
        doc(
            4,
            "config/app.yaml",
            "configuration",
            None,
            None,
            "session_timeout: 30\nrefresh: true\n",
        ),
    ]
}

fn ids(index: &LexicalIndex, query: &Query) -> Vec<u64> {
    index
        .search(query, 50, 0)
        .unwrap()
        .hits
        .iter()
        .map(|h| h.id)
        .collect()
}

fn sorted(mut v: Vec<u64>) -> Vec<u64> {
    v.sort();
    v
}

#[test]
fn tokens_split_camel_snake_and_chains_like_the_spec_says() {
    // INDEXING §18.3, row by row.
    let texts = |s: &str| -> Vec<String> { code_tokens(s).into_iter().map(|t| t.text).collect() };
    assert_eq!(texts("camelCase"), ["camel", "case", "camelcase"]);
    assert_eq!(texts("snake_case"), ["snake", "case", "snake_case"]);
    assert_eq!(
        texts("qualified::name"),
        ["qualified", "qualified::name", "name"]
    );
    assert_eq!(texts("foo.bar"), ["foo", "foo.bar", "bar"]);
    assert_eq!(
        texts("parseHTTPRequest"),
        ["parse", "http", "request", "parsehttprequest"]
    );
    // A space breaks a chain.
    assert_eq!(texts("a :: b"), ["a", "b"]);
}

#[test]
fn a_camel_case_part_finds_the_whole_identifier() {
    let index = LexicalIndex::build(None, corpus()).unwrap();
    let hits = ids(&index, &Query::new(QueryMode::Tokens, "session"));
    assert_eq!(hits[0], 1, "the symbol named for it ranks first: {hits:?}");
    assert_eq!(sorted(hits), [1, 3, 4]);
    // `config/app.yaml` has it in its path.
    assert_eq!(
        sorted(ids(&index, &Query::new(QueryMode::Tokens, "config"))),
        [2, 4]
    );
}

#[test]
fn identifier_and_qualified_queries_are_exact() {
    let index = LexicalIndex::build(None, corpus()).unwrap();
    assert_eq!(
        ids(&index, &Query::new(QueryMode::Identifier, "load_config")),
        [2]
    );
    // Not a part of a word.
    assert!(ids(&index, &Query::new(QueryMode::Identifier, "load")).is_empty());
    let q = Query::new(QueryMode::Qualified, "auth::session::refreshSession");
    let hits = ids(&index, &q);
    assert_eq!(hits[0], 1, "the declaration ranks above the use: {hits:?}");
    assert_eq!(sorted(hits), [1, 3]);
}

#[test]
fn paths_match_exactly_or_by_their_parts() {
    let index = LexicalIndex::build(None, corpus()).unwrap();
    assert_eq!(
        ids(&index, &Query::new(QueryMode::Path, "src/config.rs")),
        [2]
    );
    assert_eq!(
        sorted(ids(&index, &Query::new(QueryMode::Path, "auth session"))),
        [1]
    );
    let mut q = Query::new(QueryMode::Tokens, "refresh");
    q.path_prefix = Some("src/".into());
    assert_eq!(ids(&index, &q), [1]);
}

#[test]
fn a_phrase_needs_its_words_in_order() {
    let index = LexicalIndex::build(None, corpus()).unwrap();
    assert_eq!(
        ids(
            &index,
            &Query::new(QueryMode::Phrase, "let total = compute")
        ),
        [1]
    );
    // Punctuation isn't part of a phrase: doc 1's `total = compute` matches.
    assert_eq!(
        ids(&index, &Query::new(QueryMode::Phrase, "total compute")),
        [1]
    );
    assert_eq!(
        ids(&index, &Query::new(QueryMode::Phrase, "compute total")),
        [3]
    );
    // Both words are in doc 1, but not in this order.
    assert!(ids(&index, &Query::new(QueryMode::Phrase, "compute let")).is_empty());
    // A phrase matches whole words, not parts: `session` alone isn't a word in doc 1.
    assert!(ids(&index, &Query::new(QueryMode::Phrase, "refresh session")).is_empty());
}

#[test]
fn role_and_language_filters_narrow_before_counting() {
    let index = LexicalIndex::build(None, corpus()).unwrap();
    let mut q = Query::new(QueryMode::Tokens, "refresh");
    q.role = Some("test".into());
    let results = index.search(&q, 50, 0).unwrap();
    assert_eq!((results.total, results.hits[0].id), (1, 3));
    let mut q = Query::new(QueryMode::Tokens, "refresh");
    q.language = Some("rust".into());
    assert_eq!(sorted(ids(&index, &q)), [1, 3]);
}

#[test]
fn an_empty_result_differs_from_a_truncated_one() {
    let index = LexicalIndex::build(None, corpus()).unwrap();
    let none = index
        .search(&Query::new(QueryMode::Tokens, "nonexistent"), 50, 0)
        .unwrap();
    assert_eq!((none.total, none.truncated, none.hits.len()), (0, false, 0));
    let page = index
        .search(&Query::new(QueryMode::Tokens, "session"), 1, 0)
        .unwrap();
    assert_eq!((page.total, page.truncated, page.hits.len()), (3, true, 1));
    let last = index
        .search(&Query::new(QueryMode::Tokens, "session"), 1, 2)
        .unwrap();
    assert_eq!((last.truncated, last.hits.len()), (false, 1));
    // Asking for more than the cap is clamped, and says so.
    let docs: Vec<LexicalDoc> = (0..600)
        .map(|i| doc(i, "a.rs", "source", None, None, "same word"))
        .collect();
    let big = LexicalIndex::build(None, docs).unwrap();
    let r = big
        .search(&Query::new(QueryMode::Tokens, "same"), 10_000, 0)
        .unwrap();
    assert_eq!((r.hits.len(), r.total, r.truncated), (500, 600, true));
}

#[test]
fn a_query_with_no_words_is_an_error_not_zero_results() {
    let index = LexicalIndex::build(None, corpus()).unwrap();
    for mode in [
        QueryMode::Tokens,
        QueryMode::Identifier,
        QueryMode::Phrase,
        QueryMode::Path,
    ] {
        assert_eq!(
            index.search(&Query::new(mode, " ::  "), 10, 0).unwrap_err(),
            SearchError::EmptyQuery,
            "{mode:?}"
        );
    }
}

#[test]
fn the_same_documents_rank_the_same_way_every_time() {
    let a = LexicalIndex::build(None, corpus()).unwrap();
    let b = LexicalIndex::build(None, corpus().into_iter().collect::<Vec<_>>()).unwrap();
    for text in ["session", "refresh", "config total"] {
        let q = Query::new(QueryMode::Tokens, text);
        assert_eq!(
            a.search(&q, 50, 0).unwrap(),
            b.search(&q, 50, 0).unwrap(),
            "{text}"
        );
    }
}

#[test]
fn an_index_on_disk_reopens_with_the_same_results() {
    let dir = TempDir::new("lexical");
    let built = LexicalIndex::build(Some(dir.path()), corpus()).unwrap();
    let q = Query::new(QueryMode::Tokens, "session");
    let reopened = LexicalIndex::open(dir.path()).unwrap();
    assert_eq!(
        reopened.search(&q, 50, 0).unwrap(),
        built.search(&q, 50, 0).unwrap()
    );
}

#[test]
fn canary_errors_never_show_a_local_path() {
    for canary in gitcko_redact::canaries() {
        let name: String = canary
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        let dir = TempDir::new("lexical-canary");
        let missing = dir.path().join(&name).join("index");
        for err in [
            LexicalIndex::open(&missing).err().unwrap(),
            LexicalIndex::build(Some(&missing), corpus()).err().unwrap(),
        ] {
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains(&name), "{shown}");
        }
    }
}

#[test]
fn a_declared_name_outranks_a_name_merely_repeated_in_content() {
    let docs = vec![
        doc(
            1,
            "a.rs",
            "source",
            None,
            None,
            "timeout timeout timeout timeout timeout",
        ),
        doc(
            2,
            "b.rs",
            "source",
            Some("timeout"),
            None,
            "fn timeout() {}",
        ),
    ];
    let index = LexicalIndex::build(None, docs).unwrap();
    assert_eq!(
        ids(&index, &Query::new(QueryMode::Identifier, "timeout"))[0],
        2
    );
    assert_eq!(ids(&index, &Query::new(QueryMode::Tokens, "timeout"))[0], 2);
}

#[test]
fn an_index_built_with_another_schema_is_refused_not_misread() {
    // Stale-index rejection (INDEXING §38.6): an older layout in the same folder.
    let dir = TempDir::new("lexical-stale");
    let mut old = tantivy::schema::Schema::builder();
    old.add_text_field("content", tantivy::schema::TEXT);
    tantivy::Index::create_in_dir(dir.path(), old.build()).unwrap();
    assert_eq!(
        LexicalIndex::open(dir.path()).err(),
        Some(SearchError::Open)
    );
}

#[test]
fn combining_accents_stay_inside_their_word() {
    // `café` spelled with a combining acute (U+0301), as some editors write it.
    let word = "cafe\u{301}";
    let texts: Vec<String> = code_tokens(&format!("{word} bar"))
        .into_iter()
        .map(|t| t.text)
        .collect();
    assert_eq!(texts, [word.to_string(), "bar".to_string()]);
    // A mark doesn't hide the camelCase boundary after the letter it sits on.
    let texts: Vec<String> = code_tokens(&format!("{word}Foo"))
        .into_iter()
        .map(|t| t.text)
        .collect();
    assert_eq!(
        texts,
        [word.to_string(), "foo".to_string(), format!("{word}foo")]
    );
    let index = LexicalIndex::build(
        None,
        vec![doc(
            1,
            "a.rs",
            "source",
            None,
            None,
            &format!("let {word} = 1;"),
        )],
    )
    .unwrap();
    assert_eq!(ids(&index, &Query::new(QueryMode::Identifier, word)), [1]);
    assert!(ids(&index, &Query::new(QueryMode::Identifier, "cafe")).is_empty());
}

#[test]
fn an_index_from_another_lexical_version_is_refused() {
    // Same fields, different tokenization: only the stored version can tell.
    let dir = TempDir::new("lexical-version");
    LexicalIndex::build(Some(dir.path()), corpus()).unwrap();
    std::fs::write(dir.path().join("lexical-version"), "0").unwrap();
    assert_eq!(
        LexicalIndex::open(dir.path()).err(),
        Some(SearchError::Open)
    );
    std::fs::remove_file(dir.path().join("lexical-version")).unwrap();
    assert_eq!(
        LexicalIndex::open(dir.path()).err(),
        Some(SearchError::Open)
    );
}

#[test]
fn a_phrase_keeps_the_gap_left_by_a_word_too_long_to_index() {
    let long = "x".repeat(256);
    let docs = vec![
        doc(
            1,
            "a.rs",
            "source",
            None,
            None,
            &format!("alpha {long} beta"),
        ),
        doc(2, "b.rs", "source", None, None, "alpha beta"),
    ];
    let index = LexicalIndex::build(None, docs).unwrap();
    assert_eq!(
        ids(
            &index,
            &Query::new(QueryMode::Phrase, format!("alpha {long} beta"))
        ),
        [1]
    );
}

#[test]
fn the_index_reports_how_many_documents_it_holds() {
    assert_eq!(LexicalIndex::build(None, corpus()).unwrap().len(), 4);
    assert!(LexicalIndex::build(None, Vec::new()).unwrap().is_empty());
}

#[test]
fn each_document_keeps_a_digest_of_what_it_was_built_from() {
    let docs = corpus();
    let index = LexicalIndex::build(None, docs.clone()).unwrap();
    let mut expected: Vec<(u64, String)> = docs.iter().map(|d| (d.id, d.digest())).collect();
    expected.sort();
    assert_eq!(index.digests().unwrap(), expected);
    // Any field changes it; the id alone doesn't, since the id is checked apart.
    let base = &docs[0];
    let changed = [
        LexicalDoc {
            path: "other.rs".into(),
            ..base.clone()
        },
        LexicalDoc {
            symbol_key: Some("k".into()),
            ..base.clone()
        },
        LexicalDoc {
            content: format!("{} ", base.content),
            ..base.clone()
        },
        LexicalDoc {
            name: None,
            qualified_name: base.name.clone(),
            ..base.clone()
        },
    ];
    for c in changed {
        assert_ne!(c.digest(), base.digest(), "{c:?}");
    }
    assert_eq!(
        LexicalDoc {
            id: 99,
            ..base.clone()
        }
        .digest(),
        base.digest()
    );
}
