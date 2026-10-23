// SPDX-License-Identifier: AGPL-3.0-or-later

//! Code-aware tokens (INDEXING §18.3): `refreshSession` is found by `refresh`,
//! `session` and `refreshsession`; `load_config` by `load` and `config`; `auth::session`
//! and `a.b` also as whole chains, so a qualified name matches exactly.

/// One token: lowercase text and the position of the word it came from. The parts of a
/// word and a chain starting at it share the word's position, so phrase queries over
/// whole words still line up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeToken {
    pub text: String,
    pub position: usize,
    pub offset_from: usize,
    pub offset_to: usize,
    pub kind: TokenKind,
}

/// What a token stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// A whole word: what exact and phrase searches match.
    Word,
    /// A piece of a word (`session` in `refreshSession`), for looser searches.
    Part,
    /// Words joined by `::` or `.`, as one token.
    Chain,
}

/// Words longer than this aren't indexed: no identifier is, and Tantivy refuses
/// oversized terms anyway.
const MAX_WORD: usize = 255;
/// Longest `a::b::c` chain kept as one token, in words.
const MAX_CHAIN: usize = 16;

/// Splits `text` into code tokens, in order of position.
pub fn code_tokens(text: &str) -> Vec<CodeToken> {
    let words = words(text);
    let mut tokens = Vec::new();
    let mut chain_start = 0;
    for (position, &(from, to)) in words.iter().enumerate() {
        let word = &text[from..to];
        let full = word.to_lowercase();
        let token = |text: String, kind| CodeToken {
            text,
            position,
            offset_from: from,
            offset_to: to,
            kind,
        };
        if word.len() <= MAX_WORD {
            for part in parts(word) {
                let part = part.to_lowercase();
                if part != full {
                    tokens.push(token(part, TokenKind::Part));
                }
            }
            tokens.push(token(full, TokenKind::Word));
        }
        // A chain ends at this word unless `::` or `.` joins it to the next one.
        let joined = words
            .get(position + 1)
            .is_some_and(|&(next, _)| matches!(&text[to..next], "::" | "."));
        if !joined {
            let length = position + 1 - chain_start;
            if (2..=MAX_CHAIN).contains(&length) {
                let (start, _) = words[chain_start];
                tokens.push(CodeToken {
                    text: text[start..to].to_lowercase(),
                    position: chain_start,
                    offset_from: start,
                    offset_to: to,
                    kind: TokenKind::Chain,
                });
            }
            chain_start = position + 1;
        }
    }
    tokens.sort_by_key(|t| t.position);
    tokens
}

/// Byte ranges of identifier-like words: runs of letters, digits and `_`.
fn words(text: &str) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        let in_word = c.is_alphanumeric() || c == '_';
        match (in_word, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                words.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        words.push((s, text.len()));
    }
    words
}

/// The parts of a word split at `_` and at case changes: `parseHTTPRequest` →
/// `parse`, `HTTP`, `Request`.
fn parts(word: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = word.char_indices().collect();
    let mut parts = Vec::new();
    let mut start = 0;
    for (k, &(i, c)) in chars.iter().enumerate() {
        if c == '_' {
            if start < i {
                parts.push(&word[start..i]);
            }
            start = i + c.len_utf8();
            continue;
        }
        let prev = k.checked_sub(1).map(|p| chars[p].1);
        let next = chars.get(k + 1).map(|&(_, n)| n);
        // `aB` starts a part at B; so does `HTTPRequest` at the R of `Request`.
        let boundary = c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
            });
        if boundary && start < i {
            parts.push(&word[start..i]);
            start = i;
        }
    }
    if start < word.len() {
        parts.push(&word[start..]);
    }
    parts
}

/// The same tokens as a Tantivy tokenizer, registered on every index: as `code` with
/// every token, and as `words` without word parts, for exact and phrase matching (where
/// `load` must not match `load_config`).
#[derive(Clone)]
pub(crate) struct CodeTokenizer {
    pub(crate) parts: bool,
}

pub(crate) struct CodeTokenStream {
    tokens: Vec<tantivy::tokenizer::Token>,
    next: usize,
}

impl tantivy::tokenizer::Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> CodeTokenStream {
        let tokens = code_tokens(text)
            .into_iter()
            .filter(|t| self.parts || t.kind != TokenKind::Part)
            .map(|t| tantivy::tokenizer::Token {
                offset_from: t.offset_from,
                offset_to: t.offset_to,
                position: t.position,
                text: t.text,
                position_length: 1,
            })
            .collect();
        CodeTokenStream { tokens, next: 0 }
    }
}

impl tantivy::tokenizer::TokenStream for CodeTokenStream {
    fn advance(&mut self) -> bool {
        self.next += 1;
        self.next <= self.tokens.len()
    }

    fn token(&self) -> &tantivy::tokenizer::Token {
        &self.tokens[self.next - 1]
    }

    fn token_mut(&mut self) -> &mut tantivy::tokenizer::Token {
        &mut self.tokens[self.next - 1]
    }
}
