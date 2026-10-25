// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, BoostQuery, Occur, PhraseQuery, RegexQuery, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::{Index, IndexReader, TantivyDocument, Term, doc};

use crate::tokenize::{CodeTokenizer, TokenKind, code_tokens};

/// Changes whenever the fields or tokenization change, so an index built by another
/// version is rebuilt rather than read (INDEXING §6).
pub const LEXICAL_SCHEMA_VERSION: u32 = 1;

/// The file beside an on-disk index recording [`LEXICAL_SCHEMA_VERSION`]. Tantivy stores
/// field layouts but not what a tokenizer does, so this is what notices an index built
/// with older tokenization.
const VERSION_FILE: &str = "lexical-version";

/// Most results one query returns (INDEXING §35.2); asking for more is clamped, and the
/// result says it was truncated.
pub const MAX_RESULTS: usize = 500;

/// One searchable unit, usually a chunk. The index crate fills it in; this crate only
/// indexes the fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LexicalDoc {
    /// The caller's id for it, returned in [`Hit::id`].
    pub id: u64,
    /// Repository-relative path, `/`-separated.
    pub path: String,
    pub language: Option<String>,
    /// File role (`source`, `test`, …).
    pub role: String,
    /// Chunk kind (`symbol`, `file_preamble`, …).
    pub kind: String,
    pub symbol_key: Option<String>,
    /// The symbol's own name, exactly as declared.
    pub name: Option<String>,
    /// The symbol's qualified name (`auth::session::refresh`).
    pub qualified_name: Option<String>,
    pub signature: Option<String>,
    /// The text to search.
    pub content: String,
}

/// How a query's text is matched (INDEXING §18.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    /// Any of the query's code tokens, ranked by BM25 over content, names, signatures and
    /// paths; a document matching in more fields ranks higher.
    Tokens,
    /// An exact identifier: a symbol named exactly this, or the whole word in content.
    Identifier,
    /// An exact qualified name (`a::b::c`).
    Qualified,
    /// An exact path, or every token of the query in the path.
    Path,
    /// These words, in this order, in content. Punctuation between them isn't checked:
    /// an exact literal is confirmed against the source by the caller.
    Phrase,
}

/// A search, with optional filters that narrow it before counting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub mode: QueryMode,
    pub text: String,
    pub language: Option<String>,
    pub role: Option<String>,
    /// Only documents whose path starts with this.
    pub path_prefix: Option<String>,
}

impl Query {
    /// A query with no filters.
    pub fn new(mode: QueryMode, text: impl Into<String>) -> Query {
        Query {
            mode,
            text: text.into(),
            language: None,
            role: None,
            path_prefix: None,
        }
    }
}

/// One matching document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub id: u64,
    /// BM25 score: comparable within one query only.
    pub score: f32,
}

/// The matches on one page, and how many there are in all.
#[derive(Debug, Clone, PartialEq)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// Every match, not just this page. Zero here, with the index complete, means none.
    pub total: usize,
    /// Matches exist beyond this page.
    pub truncated: bool,
}

/// What went wrong. Tantivy's own errors can name local files, so they're dropped and
/// only the step that failed is reported.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("the lexical index can't be built")]
    Build,
    #[error("the lexical index can't be opened, or is corrupt")]
    Open,
    #[error("the lexical index can't be read")]
    Read,
    #[error("the query has no words to search for")]
    EmptyQuery,
}

/// The fields, looked up once.
#[derive(Clone, Copy)]
struct Fields {
    id: Field,
    path: Field,
    path_tokens: Field,
    language: Field,
    role: Field,
    kind: Field,
    symbol_key: Field,
    name: Field,
    name_exact: Field,
    qualified: Field,
    signature: Field,
    /// Content with word parts, for token searches.
    content: Field,
    /// Content as whole words and chains only, with positions, for exact and phrase
    /// searches.
    content_words: Field,
}

/// A Tantivy index of [`LexicalDoc`]s, read-only once built.
pub struct LexicalIndex {
    reader: IndexReader,
    fields: Fields,
}

/// The two tokenizers every index needs; Tantivy doesn't store them with the index.
fn register(index: &Index) {
    index
        .tokenizers()
        .register("code", CodeTokenizer { parts: true });
    index
        .tokenizers()
        .register("words", CodeTokenizer { parts: false });
}

fn schema() -> (Schema, Fields) {
    let code = |tokenizer: &str, positions: bool| {
        let record = if positions {
            IndexRecordOption::WithFreqsAndPositions
        } else {
            IndexRecordOption::WithFreqs
        };
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(tokenizer)
                .set_index_option(record),
        )
    };
    let mut s = Schema::builder();
    let fields = Fields {
        id: s.add_u64_field("id", STORED),
        path: s.add_text_field("path", STRING),
        path_tokens: s.add_text_field("path_tokens", code("code", false)),
        language: s.add_text_field("language", STRING),
        role: s.add_text_field("role", STRING),
        kind: s.add_text_field("kind", STRING),
        symbol_key: s.add_text_field("symbol_key", STRING),
        name: s.add_text_field("name", code("code", false)),
        name_exact: s.add_text_field("name_exact", STRING),
        qualified: s.add_text_field("qualified", STRING),
        signature: s.add_text_field("signature", code("code", false)),
        content: s.add_text_field("content", code("code", false)),
        content_words: s.add_text_field("content_words", code("words", true)),
    };
    (s.build(), fields)
}

impl LexicalIndex {
    /// Indexes `docs` into `dir` (an empty folder), or in memory when `dir` is `None`.
    /// One writer thread and one commit, so the same documents always give the same
    /// index and the same ranking.
    pub fn build(
        dir: Option<&Path>,
        docs: impl IntoIterator<Item = LexicalDoc>,
    ) -> Result<LexicalIndex, SearchError> {
        let (schema, f) = schema();
        let index = match dir {
            Some(dir) => Index::create_in_dir(dir, schema).map_err(|_| SearchError::Build)?,
            None => Index::create_in_ram(schema),
        };
        register(&index);
        let mut writer = index
            .writer_with_num_threads::<TantivyDocument>(1, 50_000_000)
            .map_err(|_| SearchError::Build)?;
        for d in docs {
            let mut document = doc!(
                f.id => d.id,
                f.path => d.path.clone(),
                f.path_tokens => d.path,
                f.role => d.role,
                f.kind => d.kind,
                f.content => d.content.clone(),
                f.content_words => d.content,
            );
            let mut optional = |field, value: Option<String>| {
                if let Some(value) = value {
                    document.add_text(field, value);
                }
            };
            optional(f.language, d.language);
            optional(f.symbol_key, d.symbol_key);
            optional(f.name, d.name.clone());
            optional(f.name_exact, d.name);
            optional(f.qualified, d.qualified_name);
            optional(f.signature, d.signature);
            writer
                .add_document(document)
                .map_err(|_| SearchError::Build)?;
        }
        writer.commit().map_err(|_| SearchError::Build)?;
        writer
            .wait_merging_threads()
            .map_err(|_| SearchError::Build)?;
        if let Some(dir) = dir {
            std::fs::write(dir.join(VERSION_FILE), LEXICAL_SCHEMA_VERSION.to_string())
                .map_err(|_| SearchError::Build)?;
        }
        Self::with_reader(&index, f)
    }

    /// Opens an index built earlier in `dir`, refusing one from another lexical version or
    /// with other fields.
    pub fn open(dir: &Path) -> Result<LexicalIndex, SearchError> {
        let version =
            std::fs::read_to_string(dir.join(VERSION_FILE)).map_err(|_| SearchError::Open)?;
        if version.trim() != LEXICAL_SCHEMA_VERSION.to_string() {
            return Err(SearchError::Open);
        }
        let index = Index::open_in_dir(dir).map_err(|_| SearchError::Open)?;
        register(&index);
        let (expected, f) = schema();
        if index.schema() != expected {
            return Err(SearchError::Open);
        }
        Self::with_reader(&index, f)
    }

    fn with_reader(index: &Index, fields: Fields) -> Result<LexicalIndex, SearchError> {
        let reader = index.reader().map_err(|_| SearchError::Open)?;
        Ok(LexicalIndex { reader, fields })
    }

    /// One page of matches: `limit` (at most [`MAX_RESULTS`]) starting at `offset`, best
    /// first. Ties keep the order documents were added in (one segment, one writer), so
    /// the same documents always come back in the same order.
    pub fn search(
        &self,
        query: &Query,
        limit: usize,
        offset: usize,
    ) -> Result<Results, SearchError> {
        let f = self.fields;
        let mut clauses = vec![(Occur::Must, self.text_query(query)?)];
        let exact = |field, value: &str| -> Box<dyn tantivy::query::Query> {
            Box::new(TermQuery::new(
                Term::from_field_text(field, value),
                IndexRecordOption::Basic,
            ))
        };
        if let Some(language) = &query.language {
            clauses.push((Occur::Must, exact(f.language, language)));
        }
        if let Some(role) = &query.role {
            clauses.push((Occur::Must, exact(f.role, role)));
        }
        if let Some(prefix) = &query.path_prefix {
            let pattern = format!("{}.*", regex::escape(prefix));
            let prefix =
                RegexQuery::from_pattern(&pattern, f.path).map_err(|_| SearchError::Read)?;
            clauses.push((Occur::Must, Box::new(prefix)));
        }
        let query = BooleanQuery::new(clauses);
        let searcher = self.reader.searcher();
        let limit = limit.min(MAX_RESULTS);
        let total = searcher
            .search(&query, &Count)
            .map_err(|_| SearchError::Read)?;
        let mut hits = Vec::new();
        if limit > 0 {
            let top = TopDocs::with_limit(limit)
                .and_offset(offset)
                .order_by_score();
            for (score, address) in searcher
                .search(&query, &top)
                .map_err(|_| SearchError::Read)?
            {
                let stored: TantivyDocument =
                    searcher.doc(address).map_err(|_| SearchError::Read)?;
                let id = stored
                    .get_first(f.id)
                    .and_then(|v| v.as_u64())
                    .ok_or(SearchError::Read)?;
                hits.push(Hit { id, score });
            }
        }
        let truncated = total > offset + hits.len();
        Ok(Results {
            hits,
            total,
            truncated,
        })
    }

    /// The part of the query that matches its text, per mode.
    fn text_query(&self, query: &Query) -> Result<Box<dyn tantivy::query::Query>, SearchError> {
        let f = self.fields;
        let term = |field, text: &str| -> Box<dyn tantivy::query::Query> {
            Box::new(TermQuery::new(
                Term::from_field_text(field, text),
                IndexRecordOption::WithFreqs,
            ))
        };
        let any = |clauses: Vec<Box<dyn tantivy::query::Query>>| -> Box<dyn tantivy::query::Query> {
            Box::new(BooleanQuery::new(
                clauses.into_iter().map(|c| (Occur::Should, c)).collect(),
            ))
        };
        let text = query.text.trim();
        let tokens = code_tokens(text);
        if tokens.is_empty() {
            return Err(SearchError::EmptyQuery);
        }
        Ok(match query.mode {
            QueryMode::Identifier => any(vec![
                term(f.name_exact, text),
                term(f.content_words, &text.to_lowercase()),
            ]),
            QueryMode::Qualified => any(vec![
                Box::new(BoostQuery::new(term(f.qualified, text), 3.0)),
                term(f.content_words, &text.to_lowercase()),
            ]),
            QueryMode::Path => {
                let all = BooleanQuery::new(
                    tokens
                        .iter()
                        .map(|t| (Occur::Must, term(f.path_tokens, &t.text)))
                        .collect(),
                );
                any(vec![
                    Box::new(BoostQuery::new(term(f.path, text), 3.0)),
                    Box::new(all),
                ])
            }
            // No per-field weights yet: a name or signature match is already an extra
            // matching clause, and weights are tuned with retrieval evals, not guessed
            // (INDEXING §20.3, IX-032).
            QueryMode::Tokens => {
                let mut texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
                texts.sort_unstable();
                texts.dedup();
                let mut clauses = Vec::new();
                for t in texts {
                    clauses.push(term(f.content, t));
                    clauses.push(term(f.name, t));
                    clauses.push(term(f.signature, t));
                    clauses.push(term(f.path_tokens, t));
                }
                any(clauses)
            }
            QueryMode::Phrase => {
                // Whole words only, one per position: what content positions hold.
                let mut words: Vec<Term> = tokens
                    .iter()
                    .filter(|t| t.kind == TokenKind::Word)
                    .map(|t| Term::from_field_text(f.content_words, &t.text))
                    .collect();
                match words.len() {
                    0 => return Err(SearchError::EmptyQuery),
                    1 => Box::new(TermQuery::new(
                        words.remove(0),
                        IndexRecordOption::WithFreqs,
                    )),
                    _ => Box::new(PhraseQuery::new(words)),
                }
            }
        })
    }
}
