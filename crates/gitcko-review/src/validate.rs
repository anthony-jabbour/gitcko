// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic validation (REVIEW_ENGINE §13): before any later stage spends a model
//! call on a candidate, check what can be checked without one. Everything a candidate
//! says about the repository is looked up in the snapshot, never taken on its word.

use std::collections::BTreeMap;

use gitcko_diff::LineKind;
use gitcko_domain::{RepoPath, Side};
use gitcko_prompts::{Candidate, SideOut};
use gitcko_protocol::review::{Evidence, OutsideDiff};
use gitcko_tools::{Ledger, Limits, has_secret, is_sensitive_name};

use crate::identity::normalise;
use crate::plan::Task;
use crate::snapshot::Snapshot;

/// A candidate that left the pipeline, and why. Codes only, so a record of it can't hold
/// repository text or model output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dropped {
    pub stage: &'static str,
    pub reason: &'static str,
}

/// Why a candidate was dropped in validation.
pub mod why {
    pub const NOT_REVIEWED: &str = "path_not_reviewed";
    pub const MALFORMED: &str = "malformed_value";
    pub const PATH_MISSING: &str = "path_missing";
    pub const LINES_MISSING: &str = "lines_missing";
    pub const NO_EVIDENCE: &str = "no_evidence";
    pub const EVIDENCE_MISSING: &str = "evidence_missing";
    pub const EVIDENCE_STALE: &str = "evidence_mismatch";
    pub const NOT_ANCHORED: &str = "not_anchored";
    pub const NOT_LINKED: &str = "outside_diff_unlinked";
}

/// A candidate that passed validation, with what the later stages need to know about it.
#[derive(Debug, Clone)]
pub struct Item {
    pub candidate: Candidate,
    /// The enclosing declaration, or the file when there is none (REVIEW_ENGINE §16).
    pub entity: String,
    /// The cited code with its whitespace collapsed: what the issue is about, which
    /// moves with the code and not with the model's wording.
    pub code: String,
    /// Set when the location is in a file this change didn't touch.
    pub outside: Option<OutsideDiff>,
    /// The changed file whose config governs this item.
    pub config_path: RepoPath,
    /// Other places the same root cause was reported.
    pub also: Vec<Evidence>,
    /// Titles of candidates on the same lines that may or may not be the same issue.
    pub overlaps: Vec<String>,
    /// The repository rule this violates, and the lowest severity it allows.
    pub rule: Option<String>,
    pub floor: Option<gitcko_prompts::Severity>,
}

/// Reads files of the snapshot's two commits, once each. A file that is a secret, a
/// symlink or binary reads as absent: nothing here may take it into a claim.
pub struct Reader<'s, 'a> {
    snapshot: &'s Snapshot<'a>,
    cache: BTreeMap<(Side, String), Option<Vec<String>>>,
}

impl<'s, 'a> Reader<'s, 'a> {
    pub fn new(snapshot: &'s Snapshot<'a>) -> Self {
        Reader {
            snapshot,
            cache: BTreeMap::new(),
        }
    }

    pub fn lines(&mut self, side: Side, path: &str) -> Option<&[String]> {
        let key = (side, path.to_string());
        if !self.cache.contains_key(&key) {
            let read = self.read(side, path);
            self.cache.insert(key.clone(), read);
        }
        self.cache[&key].as_deref()
    }

    fn read(&self, side: Side, path: &str) -> Option<Vec<String>> {
        let path = RepoPath::parse(path).ok()?;
        let commit = match side {
            Side::Old => self.snapshot.base,
            Side::New => self.snapshot.head,
        };
        let blob = self.snapshot.repo.read_blob(commit, &path).ok()??;
        if blob.symlink || blob.bytes.contains(&0) || is_sensitive_name(&path) {
            return None;
        }
        let text = String::from_utf8_lossy(&blob.bytes).into_owned();
        if has_secret(&text) {
            return None;
        }
        Some(split_lines(&text))
    }
}

/// Lines without their `\n`; a final newline doesn't make an extra empty line.
fn split_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.split('\n').map(str::to_string).collect()
}

/// Whether `text`, as a tool recorded it, is what the file has at `start..=end`: lines cut
/// to the tools' line limit, line endings ignored.
fn text_matches(lines: &[String], start: u32, end: u32, text: &str) -> bool {
    let limit = Limits::default().line_chars;
    let (Some(from), Some(to)) = (start.checked_sub(1), usize::try_from(end).ok()) else {
        return false;
    };
    let Some(have) = lines.get(from as usize..to) else {
        return false;
    };
    let cut = |line: &str| -> String {
        line.chars()
            .take(limit)
            .collect::<String>()
            .trim_end_matches('\r')
            .to_string()
    };
    let want: Vec<String> = text
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    have.iter().map(|l| cut(l)).collect::<Vec<_>>() == want
}

/// The first changed line of `path` on `side` inside `start..=end`: an added line on the
/// new side, a removed one on the old.
fn changed_in(
    snapshot: &Snapshot<'_>,
    path: &str,
    side: Side,
    start: u32,
    end: u32,
) -> Option<u32> {
    let first = |start: u32, lines: u32| if lines == 0 { start + 1 } else { start.max(1) };
    for file in &snapshot.change.files {
        let named = match side {
            Side::Old => &file.old_path,
            Side::New => &file.new_path,
        };
        if named.as_ref().map(RepoPath::as_str) != Some(path) {
            continue;
        }
        for hunk in &file.hunks {
            let (mut old, mut new) = (
                first(hunk.old_start, hunk.old_lines),
                first(hunk.new_start, hunk.new_lines),
            );
            for line in &hunk.lines {
                let here = match (line.kind, side) {
                    (LineKind::Added, Side::New) => Some(new),
                    (LineKind::Removed, Side::Old) => Some(old),
                    _ => None,
                };
                if let Some(n) = here.filter(|n| (start..=end).contains(n)) {
                    return Some(n);
                }
                if line.kind != LineKind::Added {
                    old += 1;
                }
                if line.kind != LineKind::Removed {
                    new += 1;
                }
            }
        }
    }
    None
}

/// The innermost declaration holding `line` of the head's `path`, else the file.
fn entity_of(snapshot: &Snapshot<'_>, path: &str, side: Side, line: u32) -> String {
    let graph = &snapshot.index.graph;
    let found = (side == Side::New).then(|| {
        graph
            .symbols
            .iter()
            .filter(|s| {
                graph
                    .files
                    .get(s.file)
                    .is_some_and(|f| f.path.as_str() == path)
            })
            .filter_map(|s| {
                let (start, end) = s.span.lines();
                (start as u32 <= line && line <= end as u32)
                    .then_some((end - start, &s.qualified_name))
            })
            .min()
    });
    match found.flatten() {
        Some((_, name)) => name.clone(),
        None => format!("file:{path}"),
    }
}

fn domain_side(side: SideOut) -> Side {
    match side {
        SideOut::Old => Side::Old,
        SideOut::New => Side::New,
    }
}

pub fn run(
    snapshot: &Snapshot<'_>,
    ledger: &Ledger,
    candidates: Vec<(Task, Candidate)>,
) -> (Vec<Item>, Vec<Dropped>) {
    let mut reader = Reader::new(snapshot);
    let reviewed = snapshot.investigated();
    let (mut items, mut dropped) = (Vec::new(), Vec::new());
    for (_, candidate) in candidates {
        match check(snapshot, &mut reader, &reviewed, ledger, candidate) {
            Ok(item) => items.push(item),
            Err(reason) => dropped.push(Dropped {
                stage: "validate",
                reason,
            }),
        }
    }
    (items, dropped)
}

fn check(
    snapshot: &Snapshot<'_>,
    reader: &mut Reader<'_, '_>,
    reviewed: &[&RepoPath],
    ledger: &Ledger,
    c: Candidate,
) -> Result<Item, &'static str> {
    // Written so NaN fails too.
    if !(0.0..=1.0).contains(&c.confidence) || c.start_line == 0 || c.end_line < c.start_line {
        return Err(why::MALFORMED);
    }
    let path = RepoPath::parse(&c.path).map_err(|_| why::MALFORMED)?;
    let changed = snapshot.dispositions.iter().any(|d| d.path == path);
    let inline = reviewed.contains(&&path);
    // A changed file the review set aside (held back, ignored, binary, deferred) is not
    // one it may speak of; an unchanged one can be, if it traces back to the change.
    if changed && !inline {
        return Err(why::NOT_REVIEWED);
    }
    let side = domain_side(c.side);
    let lines = reader.lines(side, &c.path).ok_or(why::PATH_MISSING)?;
    if c.end_line as usize > lines.len() {
        return Err(why::LINES_MISSING);
    }
    // What the issue is about: the first changed line in the cited range (the range's
    // first line if none changed), so a model citing a few more lines of context, or the
    // file gaining lines above, names the same issue.
    let anchor =
        changed_in(snapshot, &c.path, side, c.start_line, c.end_line).unwrap_or(c.start_line);
    let code = normalise(&lines[anchor as usize - 1]);
    if c.evidence.is_empty() {
        return Err(why::NO_EVIDENCE);
    }
    let mut cited = Vec::new();
    for id in &c.evidence {
        let e = ledger.get(id).ok_or(why::EVIDENCE_MISSING)?;
        let seen = reader
            .lines(e.range.side(), e.path.as_str())
            .ok_or(why::EVIDENCE_STALE)?;
        if !text_matches(seen, e.range.start(), e.range.end(), &e.text) {
            return Err(why::EVIDENCE_STALE);
        }
        cited.push(e);
    }
    // The lowest (path, line) among the cited ranges that sit on a changed line of a
    // reviewed file: what ties the claim to this change.
    let link = cited
        .iter()
        .filter(|e| reviewed.contains(&&e.path))
        .filter_map(|e| {
            changed_in(
                snapshot,
                e.path.as_str(),
                e.range.side(),
                e.range.start(),
                e.range.end(),
            )
            .map(|line| (e.path.clone(), line))
        })
        .min();
    let (outside, config_path) = if inline {
        link.ok_or(why::NOT_ANCHORED)?;
        (None, path)
    } else {
        let own = cited.iter().any(|e| {
            e.path == path
                && e.range.side() == side
                && e.range.start() <= c.end_line
                && c.start_line <= e.range.end()
        });
        let (linked, line) = link.filter(|_| own).ok_or(why::NOT_LINKED)?;
        let outside = OutsideDiff {
            linked_path: linked.as_str().to_string(),
            linked_line: line,
        };
        (Some(outside), linked)
    };
    Ok(Item {
        entity: entity_of(snapshot, &c.path, side, c.start_line),
        code,
        outside,
        config_path,
        also: Vec::new(),
        overlaps: Vec::new(),
        rule: None,
        floor: None,
        candidate: c,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| (*l).to_string()).collect()
    }

    #[test]
    fn evidence_matches_the_file_it_was_read_from() {
        let lines = file(&["a", "b", "c"]);
        assert!(text_matches(&lines, 1, 3, "a\nb\nc"));
        assert!(text_matches(&lines, 2, 2, "b"));
        // Wrong text, wrong place, past the end: all different from what was read.
        assert!(!text_matches(&lines, 2, 2, "c"));
        assert!(!text_matches(&lines, 1, 2, "b\nc"));
        assert!(!text_matches(&lines, 3, 4, "c\nd"));
        assert!(!text_matches(&lines, 0, 1, "a"));
        assert!(!text_matches(&lines, 1, 3, "a\nb"));
    }

    #[test]
    fn line_endings_and_the_tools_line_cut_do_not_count_as_a_change() {
        let long = "x".repeat(Limits::default().line_chars + 50);
        let cut = "x".repeat(Limits::default().line_chars);
        assert!(text_matches(&file(&[&long]), 1, 1, &cut));
        assert!(!text_matches(&file(&[&long]), 1, 1, &long));
        assert!(text_matches(&file(&["a\r", "b"]), 1, 2, "a\nb\r"));
    }

    #[test]
    fn a_file_is_its_lines_without_the_final_newline() {
        assert!(split_lines("").is_empty());
        assert_eq!(split_lines("a\nb\n"), ["a", "b"]);
        assert_eq!(split_lines("a\n\n"), ["a", ""]);
        assert_eq!(split_lines("a"), ["a"]);
    }
}

#[cfg(test)]
pub mod fixtures {
    use gitcko_prompts::{Candidate, Category, CommentType, Severity, SideOut};

    use super::Item;

    /// An item at `path:start-end`, for the stages after validation.
    pub fn item(path: &str, title: &str, category: Category, lines: (u32, u32)) -> Item {
        Item {
            candidate: Candidate {
                title: title.into(),
                category,
                severity: Severity::P1,
                comment_type: CommentType::Logic,
                confidence: 0.9,
                path: path.into(),
                side: SideOut::New,
                start_line: lines.0,
                end_line: lines.1,
                trigger: "t".into(),
                cause: "c".into(),
                impact: "i".into(),
                evidence: vec!["e1".into()],
                counterevidence: Vec::new(),
                suggestion: None,
            },
            entity: "f".into(),
            code: "x / y".into(),
            outside: None,
            config_path: gitcko_domain::RepoPath::parse(path).unwrap(),
            also: Vec::new(),
            overlaps: Vec::new(),
            rule: None,
            floor: None,
        }
    }
}
