// SPDX-License-Identifier: AGPL-3.0-or-later

//! The read-only tools a review investigator may call (REVIEW_ENGINE §10), bound to one
//! snapshot: the base and head commits, the change between them and the head's index.
//!
//! Every tool reads git objects or the index, never the working tree or the network, and
//! runs nothing. Inputs are model output, so they're parsed into closed types first
//! ([`ToolCall`]) and every path goes through [`RepoPath`] (no `..`, no absolute paths).
//! Symlinks are reported, never followed. Every result is bounded and says whether it
//! was cut short, and "nothing found" is kept apart from "couldn't look" (`unavailable`)
//! and from what a partial index couldn't see (`coverage`). Files that look like secrets
//! are held back from every tool (CLI_SPEC §3.3).
//!
//! The tools are synchronous and fast (bounded reads of the object database and the
//! in-memory index), so the engine calls them between model calls and checks its
//! deadline and cancellation around each one.

mod call;
mod evidence;
mod sensitive;

use std::collections::HashMap;

use serde_json::{Value, json};

use gitcko_diff::{ChangeSet, LineKind};
use gitcko_domain::{Coverage, LineRange, RepoPath, Side};
use gitcko_git::{CommitId, Repo};
use gitcko_graph::{Endpoint, Target};
use gitcko_index::{CodeQuery, CodeSearchMode, Relations, Revision};
use gitcko_search::QueryMode;

pub use call::{
    CallError, FindSymbol, GitLog, NAMES, NoArgs, PathArg, ReadDiff, ReadFile, RetrieveChunks,
    SearchCode, SearchMode, SideArg, SymbolArg, ToolCall, definitions,
};
pub use evidence::{Evidence, Ledger};
pub use sensitive::{has_secret, is_sensitive_name};

/// How much one call may return. Defaults suit a Base review; they are review policy,
/// not user config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Lines one `read_file` returns.
    pub read_lines: u32,
    /// Characters kept of one line; longer lines are cut and the result says so.
    pub line_chars: usize,
    /// Results in one list (matches, symbols, edges, commits, chunks).
    pub results: usize,
    /// Bytes a literal search may scan.
    pub scan_bytes: u64,
    /// Commits `git_log` may look at.
    pub log_scan: usize,
    /// Largest file a read loads; a bigger one is `unavailable` without being read.
    pub file_bytes: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            read_lines: 200,
            line_chars: 400,
            results: 20,
            scan_bytes: 8 * 1024 * 1024,
            log_scan: 500,
            file_bytes: 1024 * 1024,
        }
    }
}

/// The outcome class of a call. Distinct so a model (and the completeness check) can
/// tell "there is none" from "couldn't be read".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    /// The path, symbol or file isn't there in the bound snapshot.
    NotFound,
    /// It exists but can't be read as text: binary, a symlink, or an index error.
    Unavailable,
    /// It looks like a secret and isn't shown (CLI_SPEC §3.3).
    HeldBack,
    /// The call was refused before running: unknown tool or bad arguments.
    Invalid,
}

/// One call's result, as sent back to the model (`body`) plus what the engine tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub status: Status,
    /// The result was cut by a limit; more exists.
    pub truncated: bool,
    /// For index-backed answers: what the index could see. Anything but `complete`
    /// means an empty answer isn't proof of none.
    pub coverage: Option<Coverage>,
    /// The JSON the model sees. Holds repository text, so it's untrusted and goes into
    /// the prompt fenced (gitcko-prompts), never into logs.
    pub body: Value,
}

impl ToolOutput {
    fn ok(body: Value, truncated: bool, coverage: Option<Coverage>) -> ToolOutput {
        ToolOutput {
            status: Status::Ok,
            truncated,
            coverage,
            body,
        }
    }

    /// A non-ok result with a fixed reason code, never echoing the input.
    fn fail(status: Status, reason: &'static str) -> ToolOutput {
        ToolOutput {
            status,
            truncated: false,
            coverage: None,
            body: json!({ "status": status, "reason": reason }),
        }
    }

    /// The JSON text for the tool message: `body` with `status`, `truncated` and
    /// `coverage` alongside.
    pub fn to_message(&self) -> String {
        let mut body = self.body.clone();
        if let Value::Object(map) = &mut body {
            map.insert("status".into(), json!(self.status));
            map.insert("truncated".into(), json!(self.truncated));
            if let Some(c) = &self.coverage {
                map.insert(
                    "coverage".into(),
                    serde_json::to_value(c).expect("coverage is JSON"),
                );
            }
        }
        body.to_string()
    }
}

/// The tools for one review: a snapshot, its index, and the evidence observed so far.
pub struct Tools<'a> {
    repo: &'a Repo,
    index: &'a Revision,
    change: &'a ChangeSet,
    base: CommitId,
    head: CommitId,
    limits: Limits,
    ledger: Ledger,
}

impl<'a> Tools<'a> {
    /// Tools over `base..head`. `index` must be the head commit's revision.
    pub fn new(
        repo: &'a Repo,
        index: &'a Revision,
        change: &'a ChangeSet,
        base: CommitId,
        head: CommitId,
        limits: Limits,
    ) -> Tools<'a> {
        Tools {
            repo,
            index,
            change,
            base,
            head,
            limits,
            ledger: Ledger::default(),
        }
    }

    /// Everything tools have returned so far, for validating cited evidence.
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Parses and runs one call. A call that can't be parsed is an `invalid` result for
    /// the model to correct, not an error for the engine.
    pub fn run_raw(&mut self, name: &str, args: &str) -> ToolOutput {
        match ToolCall::parse(name, args) {
            Ok(call) => self.run(&call),
            Err(CallError::UnknownTool) => ToolOutput::fail(Status::Invalid, "unknown_tool"),
            Err(CallError::TooLarge) => ToolOutput::fail(Status::Invalid, "arguments_too_large"),
            Err(CallError::BadArguments) => ToolOutput::fail(Status::Invalid, "bad_arguments"),
        }
    }

    /// Runs one parsed call.
    pub fn run(&mut self, call: &ToolCall) -> ToolOutput {
        let mut out = self.dispatch(call);
        // File content is held back whole before this point; this catches the rest of
        // the repository-derived strings: commit subjects, symbol, crate and file names.
        scrub(&mut out.body);
        out
    }

    fn dispatch(&mut self, call: &ToolCall) -> ToolOutput {
        match call {
            ToolCall::ReadFile(a) => self.read_file(a),
            ToolCall::ReadDiff(a) => self.read_diff(a),
            ToolCall::SearchCode(a) => self.search_code(a),
            ToolCall::FindSymbol(a) => self.find_symbol(a),
            ToolCall::FindReferences(a) => self.relations(a, Revision::find_references),
            ToolCall::FindCallers(a) => self.relations(a, Revision::find_callers),
            ToolCall::FindCallees(a) => self.relations(a, Revision::find_callees),
            ToolCall::FindImporters(a) => self.find_importers(a),
            ToolCall::FindTests(a) => self.find_tests(a),
            ToolCall::GitLog(a) => self.git_log(a),
            ToolCall::RetrieveChunks(a) => self.retrieve_chunks(a),
            ToolCall::RetrieveRepoSummary(_) => self.repo_summary(),
        }
    }

    fn commit(&self, side: Side) -> CommitId {
        match side {
            Side::Old => self.base,
            Side::New => self.head,
        }
    }

    /// The text of `path` at `side`, or why not. Held back by name before any read, and
    /// by content after.
    fn text(&self, path: &RepoPath, side: Side) -> Result<String, ToolOutput> {
        if is_sensitive_name(path) {
            return Err(ToolOutput::fail(Status::HeldBack, "sensitive_file"));
        }
        let commit = self.commit(side);
        // Checked from the object header, so a huge file is never loaded.
        match self.repo.file_size(commit, path) {
            Ok(Some(size)) if size > self.limits.file_bytes => {
                return Err(ToolOutput::fail(Status::Unavailable, "too_large"));
            }
            Ok(_) => {}
            Err(_) => return Err(ToolOutput::fail(Status::Unavailable, "read_failed")),
        }
        let blob = match self.repo.read_blob(commit, path) {
            Ok(Some(blob)) => blob,
            Ok(None) => return Err(ToolOutput::fail(Status::NotFound, "no_such_file")),
            Err(_) => return Err(ToolOutput::fail(Status::Unavailable, "read_failed")),
        };
        if blob.symlink {
            return Err(ToolOutput::fail(Status::Unavailable, "symlink"));
        }
        if blob.bytes.contains(&0) {
            return Err(ToolOutput::fail(Status::Unavailable, "binary"));
        }
        let text = String::from_utf8_lossy(&blob.bytes).into_owned();
        if has_secret(&text) {
            return Err(ToolOutput::fail(Status::HeldBack, "secret_in_content"));
        }
        Ok(text)
    }

    /// Lines `start..=end` (clamped to the file), each cut to the line limit, recorded
    /// as one piece of evidence. Returns the evidence id, the numbered text and whether
    /// any line was cut.
    fn observe(
        &mut self,
        path: &RepoPath,
        side: Side,
        lines: &[&str],
        start: u32,
        end: u32,
    ) -> (String, String, bool) {
        let mut cut = false;
        let mut shown = Vec::new();
        let mut numbered = String::new();
        for n in start..=end {
            let line = lines[(n - 1) as usize];
            let kept: String = line.chars().take(self.limits.line_chars).collect();
            cut |= kept.len() < line.len();
            numbered.push_str(&format!("{n:>5}| {kept}\n"));
            shown.push(kept);
        }
        let range = LineRange::new(side, start, end).expect("1 <= start <= end");
        let id = self.ledger.record(path, range, shown.join("\n"));
        (id, numbered, cut)
    }

    fn read_file(&mut self, args: &ReadFile) -> ToolOutput {
        let Ok(path) = RepoPath::parse(&args.path) else {
            return ToolOutput::fail(Status::Invalid, "bad_path");
        };
        let side = side(args.side.unwrap_or(SideArg::New));
        let text = match self.text(&path, side) {
            Ok(text) => text,
            Err(out) => return out,
        };
        let lines: Vec<&str> = split_lines(&text);
        let total = lines.len() as u32;
        let start = args.start_line.unwrap_or(1).max(1);
        if total == 0 || start > total {
            return ToolOutput::ok(
                json!({ "path": path.as_str(), "side": side_name(side), "total_lines": total, "lines": "" }),
                false,
                None,
            );
        }
        let wanted = args.end_line.unwrap_or(u32::MAX).max(start).min(total);
        let end = wanted.min(start + self.limits.read_lines - 1);
        let (id, numbered, cut) = self.observe(&path, side, &lines, start, end);
        ToolOutput::ok(
            json!({
                "path": path.as_str(),
                "side": side_name(side),
                "start_line": start,
                "end_line": end,
                "total_lines": total,
                "evidence": id,
                "lines": numbered,
            }),
            end < wanted || cut,
            None,
        )
    }

    fn read_diff(&mut self, args: &ReadDiff) -> ToolOutput {
        let Ok(path) = RepoPath::parse(&args.path) else {
            return ToolOutput::fail(Status::Invalid, "bad_path");
        };
        let Some(file) = self
            .change
            .files
            .iter()
            .find(|f| f.new_path.as_ref() == Some(&path) || f.old_path.as_ref() == Some(&path))
        else {
            return ToolOutput::fail(Status::NotFound, "not_in_change");
        };
        let shown = file
            .new_path
            .as_ref()
            .or(file.old_path.as_ref())
            .expect("one side");
        if is_sensitive_name(shown) || file.old_path.as_ref().is_some_and(is_sensitive_name) {
            return ToolOutput::fail(Status::HeldBack, "sensitive_file");
        }
        if file.binary {
            return ToolOutput::fail(Status::Unavailable, "binary");
        }
        // Whole files, both sides, not just the hunks: a secret's marker line can sit
        // outside every hunk while its body is inside one. A submodule has no file, and
        // its hunk is only the commit ids. Anything else that can't be read stays refused:
        // a crafted entry mode is unreadable here yet still has a diff.
        for (side, p) in [(Side::Old, &file.old_path), (Side::New, &file.new_path)] {
            if let Some(p) = p
                && let Err(out) = self.text(p, side)
                && !self
                    .repo
                    .is_submodule(self.commit(side), p)
                    .unwrap_or(false)
            {
                return out;
            }
        }
        let mut hunks = Vec::new();
        let mut cut = false;
        // One budget for every hunk's lines, as for a read: a generated file is one hunk.
        let mut budget = self.limits.read_lines;
        for (i, h) in file.hunks.iter().enumerate() {
            if i == self.limits.results || budget == 0 {
                cut = true;
                break;
            }
            let mut text = String::new();
            let (mut old_n, mut new_n) = (
                first(h.old_start, h.old_lines),
                first(h.new_start, h.new_lines),
            );
            let (mut old_lines, mut new_lines) = (Vec::new(), Vec::new());
            for line in &h.lines {
                if budget == 0 {
                    cut = true;
                    break;
                }
                budget -= 1;
                let kept: String = line.text.chars().take(self.limits.line_chars).collect();
                cut |= kept.len() < line.text.len();
                let (marker, o, n) = match line.kind {
                    LineKind::Context => (' ', Some(old_n), Some(new_n)),
                    LineKind::Removed => ('-', Some(old_n), None),
                    LineKind::Added => ('+', None, Some(new_n)),
                };
                let num = |v: Option<u32>| v.map_or("     ".to_string(), |v| format!("{v:>5}"));
                text.push_str(&format!("{} {} {marker}{kept}\n", num(o), num(n)));
                if o.is_some() {
                    old_lines.push(kept.clone());
                    old_n += 1;
                }
                if n.is_some() {
                    new_lines.push(kept);
                    new_n += 1;
                }
            }
            let mut ev = |side: Side, start: u32, lines: Vec<String>| {
                (!lines.is_empty()).then(|| {
                    let end = start + lines.len() as u32 - 1;
                    let range = LineRange::new(side, start, end).expect("hunk lines start at 1");
                    self.ledger
                        .record(shown_path(file, side), range, lines.join("\n"))
                })
            };
            let old_ev = ev(Side::Old, first(h.old_start, h.old_lines), old_lines);
            let new_ev = ev(Side::New, first(h.new_start, h.new_lines), new_lines);
            hunks.push(json!({
                "header": format!("@@ -{},{} +{},{} @@", h.old_start, h.old_lines, h.new_start, h.new_lines),
                "old_evidence": old_ev,
                "new_evidence": new_ev,
                "lines": text,
            }));
        }
        ToolOutput::ok(
            json!({
                "old_path": file.old_path.as_ref().map(RepoPath::as_str),
                "new_path": file.new_path.as_ref().map(RepoPath::as_str),
                "hunks": hunks,
            }),
            cut,
            None,
        )
    }

    fn search_code(&mut self, args: &SearchCode) -> ToolOutput {
        let Some(query) = bounded(&args.query) else {
            return ToolOutput::fail(Status::Invalid, "bad_query");
        };
        let mode = match args.mode.unwrap_or(SearchMode::Literal) {
            SearchMode::Literal => CodeSearchMode::Literal,
            SearchMode::Tokens => CodeSearchMode::Lexical(QueryMode::Tokens),
            SearchMode::Identifier => CodeSearchMode::Lexical(QueryMode::Identifier),
        };
        let prefix = match &args.path_prefix {
            Some(p) if p.len() > call::MAX_QUERY || p.contains("..") => {
                return ToolOutput::fail(Status::Invalid, "bad_path");
            }
            p => p.clone(),
        };
        let limit = self.limits.results;
        let q = CodeQuery {
            text: query.to_string(),
            mode,
            language: None,
            role: None,
            path_prefix: prefix,
            limit,
            offset: args.page.unwrap_or(0) as usize * limit,
        };
        let results = match self
            .index
            .search_code(self.repo, &q, self.limits.scan_bytes)
        {
            Ok(r) => r,
            Err(_) => return ToolOutput::fail(Status::Unavailable, "search_failed"),
        };
        let mut files: HashMap<RepoPath, Result<Vec<String>, Status>> = HashMap::new();
        let (mut hits, mut held_back, mut skipped, mut cut) = (Vec::new(), 0, 0, false);
        for hit in &results.hits {
            let lines = files
                .entry(hit.path.clone())
                .or_insert_with(|| {
                    self.text(&hit.path, Side::New)
                        .map(|t| split_lines(&t).into_iter().map(str::to_string).collect())
                        .map_err(|out| out.status)
                })
                .clone();
            let lines = match lines {
                Ok(lines) => lines,
                Err(Status::HeldBack) => {
                    held_back += 1;
                    continue;
                }
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            // A hit may span a whole chunk: show its first lines and say where it ends, so
            // the model knows to read on.
            let (start, span_end) = hit.span.lines();
            let span_end = (span_end as u32).min(lines.len() as u32);
            let start = start as u32;
            let end = span_end.min(start + 2);
            if start == 0 || end < start {
                skipped += 1;
                continue;
            }
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let (id, numbered, line_cut) = self.observe(&hit.path, Side::New, &refs, start, end);
            cut |= line_cut;
            hits.push(json!({ "path": hit.path.as_str(), "line": start, "end_line": span_end, "evidence": id, "lines": numbered }));
        }
        ToolOutput::ok(
            json!({ "hits": hits, "held_back": held_back, "skipped": skipped, "total": results.total }),
            results.truncated || cut || skipped > 0,
            Some(results.coverage),
        )
    }

    /// The model-facing id of graph symbol `i`.
    fn symbol_json(&self, i: usize) -> Value {
        let s = &self.index.graph.symbols[i];
        let (start, end) = s.span.lines();
        json!({
            "symbol": format!("s{i}"),
            "name": s.name,
            "qualified_name": s.qualified_name,
            "kind": format!("{:?}", s.kind).to_lowercase(),
            "path": self.index.graph.files[s.file].path.as_str(),
            "start_line": start,
            "end_line": end,
            "in_test": s.in_test,
        })
    }

    /// Parses `s12` into a symbol index that exists in this revision.
    fn symbol(&self, arg: &SymbolArg) -> Option<usize> {
        let i: usize = arg.symbol.strip_prefix('s')?.parse().ok()?;
        (i < self.index.graph.symbols.len()).then_some(i)
    }

    fn find_symbol(&mut self, args: &FindSymbol) -> ToolOutput {
        let Some(name) = bounded(&args.name) else {
            return ToolOutput::fail(Status::Invalid, "bad_name");
        };
        let path = match &args.path {
            None => None,
            Some(p) => match RepoPath::parse(p) {
                Ok(p) => Some(p),
                Err(_) => return ToolOutput::fail(Status::Invalid, "bad_path"),
            },
        };
        let found = self
            .index
            .find_symbol(name, path.as_ref(), self.limits.results);
        let symbols: Vec<Value> = found.symbols.iter().map(|&i| self.symbol_json(i)).collect();
        ToolOutput::ok(
            json!({ "symbols": symbols }),
            found.truncated,
            Some(found.coverage),
        )
    }

    fn edges_json(&self, edges: &[usize]) -> Vec<Value> {
        edges
            .iter()
            .map(|&e| {
                let edge = &self.index.graph.edges[e];
                let end = |p: &Endpoint| match p {
                    Endpoint::Symbol(s) => json!(format!("s{s}")),
                    Endpoint::File(f) => json!(self.index.graph.files[*f].path.as_str()),
                    Endpoint::Module(_, path) => json!(path.join("::")),
                };
                let to = match &edge.to {
                    Target::Resolved(t) => end(t),
                    Target::Candidates(c) => json!(c.iter().map(end).collect::<Vec<_>>()),
                    Target::Unresolved(path) => json!(path.join("::")),
                };
                json!({
                    "from": end(&edge.from),
                    "to": to,
                    "path": self.index.graph.files[edge.file].path.as_str(),
                    "line": edge.span.lines().0,
                    "confidence": format!("{:?}", edge.confidence).to_lowercase(),
                })
            })
            .collect()
    }

    fn relations(
        &mut self,
        args: &SymbolArg,
        query: fn(&Revision, usize, usize) -> Relations,
    ) -> ToolOutput {
        let Some(symbol) = self.symbol(args) else {
            return ToolOutput::fail(Status::NotFound, "no_such_symbol");
        };
        let r = query(self.index, symbol, self.limits.results);
        ToolOutput::ok(
            json!({
                "resolved": self.edges_json(&r.resolved),
                "candidates": self.edges_json(&r.candidates),
                "unresolved": self.edges_json(&r.unresolved),
            }),
            r.truncated,
            Some(r.coverage),
        )
    }

    fn find_importers(&mut self, args: &PathArg) -> ToolOutput {
        let Ok(path) = RepoPath::parse(&args.path) else {
            return ToolOutput::fail(Status::Invalid, "bad_path");
        };
        let graph = &self.index.graph;
        let Some(file) = graph.files.iter().position(|f| f.path == path) else {
            return ToolOutput::fail(Status::NotFound, "no_such_file");
        };
        // A Rust file is imported through its module, so ask about both.
        let mut targets = vec![Endpoint::File(file)];
        if let Some(unit) = graph.files[file].crate_unit {
            targets.push(Endpoint::Module(unit, graph.files[file].module.clone()));
        }
        let (mut resolved, mut candidates, mut truncated) = (Vec::new(), Vec::new(), false);
        let mut coverage = None;
        for t in &targets {
            let r = self.index.find_importers(t, self.limits.results);
            resolved.extend(r.resolved);
            candidates.extend(r.candidates);
            truncated |= r.truncated;
            coverage = Some(r.coverage);
        }
        resolved.sort_unstable();
        resolved.dedup();
        candidates.sort_unstable();
        candidates.dedup();
        ToolOutput::ok(
            json!({ "resolved": self.edges_json(&resolved), "candidates": self.edges_json(&candidates) }),
            truncated,
            coverage,
        )
    }

    fn find_tests(&mut self, args: &SymbolArg) -> ToolOutput {
        let Some(symbol) = self.symbol(args) else {
            return ToolOutput::fail(Status::NotFound, "no_such_symbol");
        };
        let found = self.index.find_tests(symbol, self.limits.results);
        let tests: Vec<Value> = found
            .tests
            .iter()
            .map(|&(i, why)| {
                let mut v = self.symbol_json(i);
                v["evidence_kind"] = json!(format!("{why:?}").to_lowercase());
                v
            })
            .collect();
        ToolOutput::ok(
            json!({ "tests": tests }),
            found.truncated,
            Some(found.coverage),
        )
    }

    fn git_log(&mut self, args: &GitLog) -> ToolOutput {
        let path = match &args.path {
            None => None,
            Some(p) => match RepoPath::parse(p) {
                Ok(p) => Some(p),
                Err(_) => return ToolOutput::fail(Status::Invalid, "bad_path"),
            },
        };
        let log = match self.repo.log(
            self.head,
            path.as_ref(),
            self.limits.results,
            self.limits.log_scan,
        ) {
            Ok(log) => log,
            Err(_) => return ToolOutput::fail(Status::Unavailable, "history_unreadable"),
        };
        let commits: Vec<Value> = log
            .entries
            .iter()
            .map(|e| json!({ "commit": e.id.to_string(), "time": e.time, "summary": e.summary }))
            .collect();
        ToolOutput::ok(
            json!({ "commits": commits, "shallow": log.shallow }),
            log.truncated,
            None,
        )
    }

    fn retrieve_chunks(&mut self, args: &RetrieveChunks) -> ToolOutput {
        let search = SearchCode {
            query: args.query.clone(),
            mode: Some(SearchMode::Tokens),
            path_prefix: None,
            page: None,
        };
        let Some(query) = bounded(&search.query) else {
            return ToolOutput::fail(Status::Invalid, "bad_query");
        };
        let q = CodeQuery {
            text: query.to_string(),
            mode: CodeSearchMode::Lexical(QueryMode::Tokens),
            language: None,
            role: None,
            path_prefix: None,
            limit: self.limits.results.min(5),
            offset: 0,
        };
        let results = match self
            .index
            .search_code(self.repo, &q, self.limits.scan_bytes)
        {
            Ok(r) => r,
            Err(_) => return ToolOutput::fail(Status::Unavailable, "search_failed"),
        };
        let (mut chunks, mut held_back, mut skipped, mut cut) = (Vec::new(), 0, 0, false);
        for chunk in results.hits.iter().filter_map(|h| h.chunk) {
            let c = &self.index.chunks[chunk];
            // The whole file, as for a read: secrets are judged on it, and evidence is
            // whole source lines even when a chunk starts mid-line.
            let text = match self.text(&c.path, Side::New) {
                Ok(text) => text,
                Err(out) => {
                    if out.status == Status::HeldBack {
                        held_back += 1;
                    } else {
                        skipped += 1;
                    }
                    continue;
                }
            };
            let lines = split_lines(&text);
            let (start, last) = c.span.lines();
            let (start, last) = (start as u32, (last as u32).min(lines.len() as u32));
            if start == 0 || last < start {
                continue;
            }
            let end = last.min(start + self.limits.read_lines - 1);
            let (id, numbered, line_cut) = self.observe(&c.path, Side::New, &lines, start, end);
            cut |= line_cut || end < last;
            chunks.push(json!({ "path": c.path.as_str(), "kind": c.kind.as_str(), "evidence": id, "lines": numbered }));
        }
        ToolOutput::ok(
            json!({ "chunks": chunks, "held_back": held_back, "skipped": skipped }),
            results.truncated || cut || skipped > 0,
            Some(results.coverage),
        )
    }

    fn repo_summary(&mut self) -> ToolOutput {
        let o = self.index.repo_overview();
        ToolOutput::ok(
            json!({
                "commit": self.index.commit,
                "files": o.files,
                "parsed_files": o.parsed_files,
                "symbols": o.symbols,
                "crates": o.crates,
                "languages": o.languages,
                // Base has no model-written summaries; only these counted facts.
                "summaries": "none",
                // The counts are of what the index saw: partial when a limit stopped it.
                "coverage": {
                    "discovery": self.index.coverage.discovery,
                    "parsing": self.index.coverage.parsing,
                },
            }),
            false,
            None,
        )
    }
}

/// Replaces credential-shaped text in every string of `value` (gitcko-redact's patterns).
fn scrub(value: &mut Value) {
    match value {
        Value::String(text) => {
            if has_secret(text) {
                *text = gitcko_redact::Redactor::new().redact(text).into_owned();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub),
        Value::Object(map) => map.values_mut().for_each(scrub),
        _ => {}
    }
}

fn side(arg: SideArg) -> Side {
    match arg {
        SideArg::Old => Side::Old,
        SideArg::New => Side::New,
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Old => "old",
        Side::New => "new",
    }
}

/// The file's path on `side`, falling back to the other for an add or delete.
fn shown_path(file: &gitcko_diff::FileDiff, side: Side) -> &RepoPath {
    let (first, second) = match side {
        Side::Old => (&file.old_path, &file.new_path),
        Side::New => (&file.new_path, &file.old_path),
    };
    first
        .as_ref()
        .or(second.as_ref())
        .expect("a diff has a path")
}

/// The first line a hunk covers on one side: git writes the line *before* an empty side.
fn first(start: u32, lines: u32) -> u32 {
    if lines == 0 { start + 1 } else { start.max(1) }
}

/// Lines without their `\n`; a final newline doesn't make an extra empty line.
fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .collect()
}

/// A non-empty query of bounded length, trimmed.
fn bounded(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty() && text.chars().count() <= call::MAX_QUERY).then_some(text)
}
