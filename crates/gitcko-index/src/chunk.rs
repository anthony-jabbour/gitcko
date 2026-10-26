// SPDX-License-Identifier: AGPL-3.0-or-later

//! Symbol-aware chunks (INDEXING §14–15): retrieval units that follow declarations, so a
//! whole function comes back as one piece and a big one comes back in ordered segments.
//! Chunks are for retrieval, never the authoritative source: exact text is always read
//! from the commit (§14, IX-038).

use gitcko_domain::{ContentHash, Coverage, Position, Reason, RepoPath, SourceSpan};
use gitcko_syntax::{FileRole, Language, SymbolKind};

/// Changes whenever chunk boundaries, headers or the policy below change, so cached
/// chunks from an older version are rebuilt (INDEXING §6, §14.3).
pub const CHUNK_SCHEMA_VERSION: u32 = 1;

/// Size bounds in bytes. INDEXING §14.3 states targets in tokens but requires byte limits
/// too, because token counts depend on the model; at roughly four bytes per token these
/// match its 850 / 1,000 / 1,400 / 100-token figures. Versioned with
/// [`CHUNK_SCHEMA_VERSION`], to be tuned by retrieval evals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkPolicy {
    /// Size a split aims for.
    pub target_bytes: usize,
    /// Most bytes in one segment of an oversized symbol.
    pub soft_max_bytes: usize,
    /// Largest symbol kept whole.
    pub hard_max_bytes: usize,
    /// Bytes repeated between consecutive segments of one split symbol (§14.4).
    pub overlap_bytes: usize,
    pub max_chunks_per_file: usize,
}

impl Default for ChunkPolicy {
    fn default() -> ChunkPolicy {
        ChunkPolicy {
            target_bytes: 3_400,
            soft_max_bytes: 4_000,
            hard_max_bytes: 5_600,
            overlap_bytes: 400,
            max_chunks_per_file: 5_000,
        }
    }
}

/// What a chunk holds (INDEXING §14.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ChunkKind {
    /// One whole declaration.
    Symbol,
    /// One piece of a declaration too big to keep whole.
    SymbolSegment,
    /// What comes before the first declaration: imports, module docs, attributes.
    FilePreamble,
    /// Text between declarations: `impl` headers, comments, braces of a split container.
    ModuleContext,
    /// A Markdown section.
    DocumentationSection,
    /// A block of a configuration or manifest file.
    ConfigurationBlock,
    /// A window of any other text.
    LexicalWindow,
}

impl ChunkKind {
    /// The name used in headers and the lexical index.
    pub fn as_str(self) -> &'static str {
        match self {
            ChunkKind::Symbol => "symbol",
            ChunkKind::SymbolSegment => "symbol_segment",
            ChunkKind::FilePreamble => "file_preamble",
            ChunkKind::ModuleContext => "module_context",
            ChunkKind::DocumentationSection => "documentation_section",
            ChunkKind::ConfigurationBlock => "configuration_block",
            ChunkKind::LexicalWindow => "lexical_window",
        }
    }
}

/// Which piece of a split declaration a segment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Segment {
    /// From 1.
    pub index: usize,
    /// How many segments the declaration has. When its file hit the chunk limit inside
    /// it, segments past the limit weren't planned and this is a lower bound.
    pub count: usize,
}

/// One chunk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Chunk {
    pub kind: ChunkKind,
    pub path: RepoPath,
    /// Exact place in the file; `text` is exactly these bytes.
    pub span: SourceSpan,
    /// The declaration it holds, as an index into the graph's symbols.
    pub symbol: Option<usize>,
    pub segment: Option<Segment>,
    /// The deterministic metadata header (§15), made without a model.
    pub header: String,
    pub text: String,
    /// SHA-256 of `text`, checked against the source when a revision is validated.
    pub content_hash: ContentHash,
}

/// A declaration the chunker may keep whole.
#[derive(Debug, Clone)]
pub struct ChunkSymbol<'a> {
    pub graph_index: usize,
    pub kind: SymbolKind,
    pub span: SourceSpan,
    pub qualified_name: &'a str,
    pub signature: Option<&'a str>,
    /// The enclosing module or type's qualified name.
    pub parent: Option<&'a str>,
}

/// The file being chunked.
#[derive(Debug, Clone, Copy)]
pub struct FileContext<'a> {
    pub path: &'a RepoPath,
    pub language: Option<Language>,
    pub role: FileRole,
}

/// Chunks a parsed source file around its declarations (§14.1–14.6). Every byte that
/// isn't whitespace ends up in a chunk, so nothing is lost between declarations.
pub fn chunk_code(
    file: &FileContext<'_>,
    source: &str,
    symbols: &[ChunkSymbol<'_>],
    policy: &ChunkPolicy,
) -> (Vec<Chunk>, Coverage) {
    let lines = LineIndex::new(source);
    let mut sorted: Vec<&ChunkSymbol<'_>> = symbols.iter().collect();
    sorted.sort_by_key(|s| (s.span.start_byte(), std::cmp::Reverse(s.span.end_byte())));
    // Pieces are planned in source order and planning stops one past the per-file limit,
    // so a tiny policy over a huge file never plans pieces it would throw away.
    let mut plan = Plan::new(policy.max_chunks_per_file);
    let mut covered_until = 0;
    for (i, symbol) in sorted.iter().enumerate() {
        if plan.full() {
            break;
        }
        let end = symbol.span.end_byte().min(source.len());
        if symbol.span.start_byte() < covered_until || symbol.span.start_byte() >= end {
            continue;
        }
        let start = with_leading_docs(source, &lines, symbol.span.start_byte(), covered_until);
        if end - start <= policy.hard_max_bytes {
            plan.text_until(source, start, policy);
            plan.push(Piece::Symbol(symbol, start, end));
            covered_until = end;
            continue;
        }
        // Too big. A container (module, trait) with declarations inside gives way to
        // them, the text around them becoming context; anything else, a function with
        // local items included, is split into segments.
        let container = matches!(symbol.kind, SymbolKind::Module | SymbolKind::Trait);
        let has_children = sorted[i + 1..].iter().any(|c| c.span.start_byte() < end);
        if container && has_children {
            continue;
        }
        plan.text_until(source, start, policy);
        let ranges = split(
            source,
            start,
            end,
            policy.soft_max_bytes,
            policy.overlap_bytes,
            plan.room(),
        );
        // Every segment knows how many there are, so a cut split is counted in full.
        let count = if ranges.last().is_some_and(|r| r.1 < end) {
            ranges.len() + 1
        } else {
            ranges.len()
        };
        for (k, (a, b)) in ranges.into_iter().enumerate() {
            plan.push(Piece::Segment(
                symbol,
                a,
                b,
                Segment {
                    index: k + 1,
                    count,
                },
            ));
        }
        covered_until = end;
    }
    if !plan.full() {
        plan.text_until(source, source.len(), policy);
    }
    finish(file, source, &lines, plan, policy)
}

/// Chunks planned so far, in source order, and where the text between them resumes.
struct Plan<'a> {
    pieces: Vec<Piece<'a>>,
    /// End of the last piece: text from here to the next piece is context.
    at: usize,
    /// No symbol yet, so text so far is the file's preamble.
    preamble: bool,
    cap: usize,
    /// A split stopped before the end of its range: text was left unplanned, even if
    /// the windows dropped were whitespace and the piece count stayed under the limit.
    cut: bool,
}

impl<'a> Plan<'a> {
    fn new(max_chunks_per_file: usize) -> Plan<'a> {
        Plan {
            pieces: Vec::new(),
            at: 0,
            preamble: true,
            cap: max_chunks_per_file,
            cut: false,
        }
    }

    /// Past the limit, or text already left out: planning stops, coverage is partial.
    fn full(&self) -> bool {
        self.cut || self.pieces.len() > self.cap
    }

    /// Plans `a..b` as text chunks of `kind`, dropping all-whitespace windows.
    fn text(&mut self, source: &str, a: usize, b: usize, kind: ChunkKind, max: usize) {
        let ranges = split(source, a, b, max, 0, self.room());
        self.cut |= ranges.last().is_some_and(|r| r.1 < b);
        for (x, y) in ranges {
            if !source[x..y].trim().is_empty() {
                self.pieces.push(Piece::Text(kind, x, y));
            }
        }
    }

    /// Pieces that can still be planned, one past the limit so a cut is visible.
    fn room(&self) -> usize {
        (self.cap + 1).saturating_sub(self.pieces.len())
    }

    fn push(&mut self, piece: Piece<'a>) {
        if !matches!(piece, Piece::Text(..)) {
            self.preamble = false;
        }
        self.at = self.at.max(piece.range().1);
        self.pieces.push(piece);
    }

    /// The text from the last piece up to `end`, trimmed to what's in it.
    fn text_until(&mut self, source: &str, end: usize, policy: &ChunkPolicy) {
        if end <= self.at {
            return;
        }
        let gap = &source[self.at..end];
        let Some(first) = gap.find(|c: char| !c.is_whitespace()) else {
            self.at = end;
            return;
        };
        let (a, b) = (self.at + first, self.at + gap.trim_end().len());
        let kind = if self.preamble {
            ChunkKind::FilePreamble
        } else {
            ChunkKind::ModuleContext
        };
        self.text(source, a, b, kind, policy.hard_max_bytes);
        self.at = end;
    }
}

/// Chunks a text file with no parser (§14.2): Markdown by section, configuration by
/// blank-line blocks, anything else by windows.
pub fn chunk_text(
    file: &FileContext<'_>,
    source: &str,
    policy: &ChunkPolicy,
) -> (Vec<Chunk>, Coverage) {
    let lines = LineIndex::new(source);
    let kind = match file.role {
        FileRole::Documentation => ChunkKind::DocumentationSection,
        FileRole::Configuration
        | FileRole::Manifest
        | FileRole::DependencyLock
        | FileRole::Workflow => ChunkKind::ConfigurationBlock,
        _ => ChunkKind::LexicalWindow,
    };
    let boundaries: Vec<usize> = match kind {
        // A new section at each Markdown heading.
        ChunkKind::DocumentationSection => (0..lines.starts.len())
            .map(|r| lines.starts[r])
            .filter(|&s| s > 0 && source[s..].starts_with('#'))
            .collect(),
        // A new block after each blank line.
        ChunkKind::ConfigurationBlock => (1..lines.starts.len())
            .map(|r| lines.starts[r])
            .filter(|&s| source[lines.starts[lines.row(s) - 1]..s].trim().is_empty())
            .collect(),
        _ => Vec::new(),
    };
    // Blocks between boundaries, then neighbours merged up to the target size, so a config
    // file isn't a hundred one-line chunks; a block over the hard maximum is windowed.
    let mut blocks = Vec::new();
    let mut at = 0;
    for cut in boundaries.into_iter().chain([source.len()]) {
        if cut > at {
            blocks.push((at, cut));
            at = cut;
        }
    }
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (a, b) in blocks {
        match merged.last_mut() {
            Some(last) if b - last.0 <= policy.target_bytes => last.1 = b,
            _ => merged.push((a, b)),
        }
    }
    let mut plan = Plan::new(policy.max_chunks_per_file);
    for (a, b) in merged {
        if plan.full() {
            break;
        }
        plan.text(source, a, b, kind, policy.hard_max_bytes);
    }
    finish(file, source, &lines, plan, policy)
}

/// Moves a declaration's start back over the doc comments and attributes written right
/// above it (Tree-sitter's span leaves them out), stopping at a blank line, other code, or
/// `floor` (the previous chunk's end). They belong with the declaration for retrieval.
///
/// Scans forward from `floor`, so a multi-line `#[derive(…)]` or `/** … */` counts whole:
/// line prefixes alone can't tell an attribute's `)]` from code.
fn with_leading_docs(source: &str, lines: &LineIndex, start: usize, floor: usize) -> usize {
    let gap = &source.as_bytes()[..start];
    let mut at = floor;
    let mut run: Option<usize> = None;
    while at < start {
        let rest = &gap[at..];
        let c = rest[0];
        if c.is_ascii_whitespace() {
            let n = rest.iter().take_while(|b| b.is_ascii_whitespace()).count();
            if rest[..n].iter().filter(|&&b| b == b'\n').count() > 1 {
                run = None;
            }
            at += n;
            continue;
        }
        // Inner docs and attributes (`//!`, `/*!`, `#!`) belong to the module, not to
        // what follows.
        let inner = rest.starts_with(b"//!") || rest.starts_with(b"/*!") || rest.starts_with(b"#!");
        let end = if rest.starts_with(b"//") {
            line_end(gap, at)
        } else if rest.starts_with(b"/*") {
            block_comment_end(gap, at)
        } else if rest.starts_with(b"#[") || rest.starts_with(b"#![") {
            attribute_end(gap, at)
        } else {
            run = None;
            at = line_end(gap, at);
            continue;
        };
        if inner {
            run = None;
        } else if run.is_none() {
            run = Some(at);
        }
        at = end.max(at + 1);
    }
    match run {
        // From its line start when only indentation comes before it.
        Some(r) => {
            let line = lines.starts[lines.row(r)];
            if line >= floor && source[line..r].trim().is_empty() {
                line
            } else {
                r
            }
        }
        None => start,
    }
}

fn line_end(s: &[u8], at: usize) -> usize {
    s[at..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(s.len(), |n| at + n)
}

/// The end of a (possibly nested) block comment starting at `at`, or the end of `s`.
fn block_comment_end(s: &[u8], at: usize) -> usize {
    let (mut i, mut depth) = (at, 0usize);
    while i + 1 < s.len() {
        match &s[i..i + 2] {
            b"/*" => {
                depth += 1;
                i += 2;
            }
            b"*/" => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return i;
                }
            }
            _ => i += 1,
        }
    }
    s.len()
}

/// The end of the attribute starting at `at` (`#[`…`]`), brackets balanced and string
/// literals skipped, or the end of `s`.
fn attribute_end(s: &[u8], at: usize) -> usize {
    let (mut i, mut depth) = (at, 0usize);
    while i < s.len() {
        match s[i] {
            b'[' => depth += 1,
            b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            b'"' => {
                // A raw string's `r#…#"` prefix is outside the quotes; count its hashes.
                let hashes = s[at..i].iter().rev().take_while(|&&b| b == b'#').count();
                let raw = i > hashes && s[i - hashes - 1] == b'r';
                i += 1;
                while i < s.len() {
                    if !raw && s[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if s[i] == b'"'
                        && (!raw
                            || s[i + 1..]
                                .iter()
                                .take(hashes)
                                .filter(|&&b| b == b'#')
                                .count()
                                == hashes)
                    {
                        i += if raw { hashes } else { 0 };
                        break;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s.len()
}

/// A planned chunk, before its header and hash are made.
enum Piece<'a> {
    Symbol(&'a ChunkSymbol<'a>, usize, usize),
    Segment(&'a ChunkSymbol<'a>, usize, usize, Segment),
    Text(ChunkKind, usize, usize),
}

impl Piece<'_> {
    fn range(&self) -> (usize, usize) {
        match *self {
            Piece::Symbol(_, a, b) | Piece::Segment(_, a, b, _) | Piece::Text(_, a, b) => (a, b),
        }
    }
}

fn finish(
    file: &FileContext<'_>,
    source: &str,
    lines: &LineIndex,
    plan: Plan<'_>,
    policy: &ChunkPolicy,
) -> (Vec<Chunk>, Coverage) {
    // Planning stopped past the limit, so how much more there was isn't known.
    let cut = plan.full();
    let chunks = plan
        .pieces
        .into_iter()
        .take(policy.max_chunks_per_file)
        .map(|piece| {
            let (a, b) = piece.range();
            let text = source[a..b].to_string();
            let (kind, symbol, segment) = match piece {
                Piece::Symbol(s, ..) => (ChunkKind::Symbol, Some(s), None),
                Piece::Segment(s, .., seg) => (ChunkKind::SymbolSegment, Some(s), Some(seg)),
                Piece::Text(kind, ..) => (kind, None, None),
            };
            Chunk {
                kind,
                path: file.path.clone(),
                span: lines.span(a, b),
                symbol: symbol.map(|s| s.graph_index),
                segment,
                header: header(file, kind, symbol, segment),
                content_hash: ContentHash::of(text.as_bytes()),
                text,
            }
        })
        .collect();
    let coverage = if cut {
        Coverage::Partial {
            reason: Reason::new("chunk_limit").expect("valid code"),
            omitted: None,
        }
    } else {
        Coverage::Complete {}
    };
    (chunks, coverage)
}

/// The metadata header of §15: language, path, kind, names, role. Built from facts the
/// index already has, never by a model, so exact names and paths are searchable even when
/// the body says little.
fn header(
    file: &FileContext<'_>,
    kind: ChunkKind,
    symbol: Option<&ChunkSymbol<'_>>,
    segment: Option<Segment>,
) -> String {
    let mut out = String::new();
    if let Some(language) = file.language {
        out += &format!("language: {}\n", language_name(language));
    }
    out += &format!("path: {}\n", file.path.as_str());
    match symbol {
        Some(s) => {
            out += &format!("kind: {}\n", symbol_kind_name(s.kind));
            out += &format!("qualified_name: {}\n", s.qualified_name);
            if let Some(signature) = s.signature {
                out += &format!("signature: {signature}\n");
            }
            if let Some(parent) = s.parent {
                out += &format!("parent: {parent}\n");
            }
        }
        None => out += &format!("kind: {}\n", kind.as_str()),
    }
    out += &format!("role: {}\n", role_name(file.role));
    if let Some(seg) = segment {
        out += &format!("segment: {}/{}\n", seg.index, seg.count);
    }
    out
}

fn language_name(language: Language) -> String {
    snake(&format!("{language:?}"))
}

fn symbol_kind_name(kind: SymbolKind) -> String {
    snake(&format!("{kind:?}"))
}

fn role_name(role: FileRole) -> String {
    snake(&format!("{role:?}"))
}

/// `TypeAlias` → `type_alias`: the spec's spelling of the enum names.
fn snake(debug: &str) -> String {
    let mut out = String::new();
    for (i, c) in debug.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Cuts `start..end` into pieces of at most `max` bytes at line ends where possible,
/// each next piece starting `overlap` bytes back (at a line start), stopping after
/// `limit` pieces. Always progresses, and always on character boundaries.
fn split(
    source: &str,
    start: usize,
    end: usize,
    max: usize,
    overlap: usize,
    limit: usize,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut s = start;
    let max = max.max(1);
    while s < end && out.len() < limit {
        let mut e = (s + max).min(end);
        if e < end {
            if let Some(nl) = source[s..floor(source, e)].rfind('\n') {
                e = s + nl + 1;
            }
            e = floor(source, e);
            if e <= s {
                e = ceil(source, s + 1).min(end);
            }
        }
        out.push((s, e));
        if e >= end {
            break;
        }
        let mut next = floor(source, e.saturating_sub(overlap));
        if next > s {
            // Start the overlap at a line start, if one falls inside it.
            if let Some(nl) = source[next..e].find('\n') {
                next += nl + 1;
            }
        }
        s = if next > s && next < e { next } else { e };
    }
    out
}

fn floor(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i.min(s.len())
}

/// Byte offset → row and column, for spans of split pieces.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> LineIndex {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        LineIndex { starts }
    }

    fn row(&self, byte: usize) -> usize {
        self.starts.partition_point(|&s| s <= byte) - 1
    }

    fn position(&self, byte: usize) -> Position {
        let row = self.row(byte);
        Position {
            row,
            column: byte - self.starts[row],
        }
    }

    fn span(&self, a: usize, b: usize) -> SourceSpan {
        SourceSpan::new(a, b, self.position(a), self.position(b)).expect("a <= b")
    }
}
