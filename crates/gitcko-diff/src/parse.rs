// SPDX-License-Identifier: AGPL-3.0-or-later

//! Parser for git's unified diff output (`git diff`, `git format-patch`).
//!
//! Strict on purpose: diff text can come from outside (an uploaded patch), so anything
//! that doesn't add up (hunk counts, hunk order, paths) is an error with a line number,
//! never a guess. Text before the first `diff --git` (a commit message, mail headers)
//! is skipped.

use gitcko_domain::RepoPath;

use crate::{Change, ChangeSet, FileDiff, Hunk, Line, LineKind, Mode, Side};

/// Why diff text couldn't be parsed, and the 1-based line where it went wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("diff line {line}: {message}")]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

type Result<T> = std::result::Result<T, ParseError>;

fn error<T>(line: usize, message: impl Into<String>) -> Result<T> {
    Err(ParseError {
        line,
        message: message.into(),
    })
}

/// Parses diff text into a [`ChangeSet`]. An empty string is an empty change set.
pub fn parse(text: &str) -> Result<ChangeSet> {
    // Split on `\n` only, so a CRLF file's `\r` stays part of its line's text.
    let lines: Vec<&str> = text.split_terminator('\n').collect();
    let mut at = 0;
    while at < lines.len() && !lines[at].starts_with("diff --git ") {
        if lines[at].starts_with("@@ ") {
            return error(at + 1, "hunk before any file header");
        }
        at += 1;
    }
    let mut files = Vec::new();
    while at < lines.len() {
        let (file, next) = parse_file(&lines, at)?;
        files.push(file);
        at = next;
    }
    Ok(ChangeSet { files })
}

/// What a file's header lines say, before deciding paths and change kind.
#[derive(Default)]
struct Header {
    git_paths: Option<(String, String)>,
    old_mode: Option<Mode>,
    new_mode: Option<Mode>,
    new_file: Option<Mode>,
    deleted_file: Option<Mode>,
    similarity: Option<u8>,
    renamed: (Option<String>, Option<String>),
    copied: (Option<String>, Option<String>),
    /// From `---`/`+++` (or a `Binary files` line): `Some(None)` is `/dev/null`.
    minus: Option<Option<String>>,
    plus: Option<Option<String>>,
    binary: bool,
}

/// Parses the file starting at `lines[start]` (a `diff --git` line). Returns it and the
/// index of the next file's first line.
fn parse_file(lines: &[&str], start: usize) -> Result<(FileDiff, usize)> {
    let header_line = start + 1;
    let mut h = Header {
        git_paths: git_header_paths(&lines[start]["diff --git ".len()..], header_line)?,
        ..Header::default()
    };
    let mut at = start + 1;
    let mut hunks = Vec::new();
    while at < lines.len() && !lines[at].starts_with("diff --git ") {
        let line = lines[at];
        let n = at + 1;
        if let Some(rest) = line.strip_prefix("old mode ") {
            h.old_mode = Some(mode(rest, n)?);
        } else if let Some(rest) = line.strip_prefix("new mode ") {
            h.new_mode = Some(mode(rest, n)?);
        } else if let Some(rest) = line.strip_prefix("new file mode ") {
            h.new_file = Some(mode(rest, n)?);
        } else if let Some(rest) = line.strip_prefix("deleted file mode ") {
            h.deleted_file = Some(mode(rest, n)?);
        } else if let Some(rest) = line.strip_prefix("similarity index ") {
            h.similarity = Some(percent(rest, n)?);
        } else if line.starts_with("dissimilarity index ") || line.starts_with("index ") {
            // Blob ids and rewrite scores aren't kept.
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            h.renamed.0 = Some(plain_path(rest, n)?);
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            h.renamed.1 = Some(plain_path(rest, n)?);
        } else if let Some(rest) = line.strip_prefix("copy from ") {
            h.copied.0 = Some(plain_path(rest, n)?);
        } else if let Some(rest) = line.strip_prefix("copy to ") {
            h.copied.1 = Some(plain_path(rest, n)?);
        } else if let Some(rest) = line.strip_prefix("Binary files ") {
            h.binary = true;
            // Only which side is /dev/null matters; the paths come from elsewhere.
            if rest.starts_with("/dev/null and ") {
                h.minus = Some(None);
            }
            if rest.ends_with(" and /dev/null differ") {
                h.plus = Some(None);
            }
            at += 1;
            break;
        } else if line == "GIT binary patch" {
            h.binary = true;
            at += 1;
            while at < lines.len() && !lines[at].starts_with("diff --git ") {
                at += 1; // the encoded content isn't kept
            }
            break;
        } else if let Some(rest) = line.strip_prefix("--- ") {
            h.minus = Some(side_path(rest, "a/", n)?);
            let Some(plus) = lines.get(at + 1).and_then(|l| l.strip_prefix("+++ ")) else {
                return error(n + 1, "expected a `+++` line after `---`");
            };
            h.plus = Some(side_path(plus, "b/", n + 1)?);
            at = parse_hunks(lines, at + 2, &mut hunks)?;
            if hunks.is_empty() {
                // git never prints these lines without a hunk after them.
                return error(n, "`---`/`+++` lines with no hunk after them");
            }
            break;
        } else if line.starts_with("@@ ") {
            return error(n, "hunk before the file's `---`/`+++` lines");
        } else {
            return error(n, format!("unexpected line in a file header: {line:?}"));
        }
        at += 1;
    }
    if at < lines.len() && !lines[at].starts_with("diff --git ") {
        return error(at + 1, "line is outside every hunk's line counts");
    }
    Ok((build(h, hunks, header_line)?, at))
}

/// Decides the paths and the kind of change from the header, checking every path.
fn build(h: Header, hunks: Vec<Hunk>, line: usize) -> Result<FileDiff> {
    let renamed_or_copied = h.renamed_or_copied();
    let added = h.new_file.is_some() || h.minus == Some(None);
    let deleted = h.deleted_file.is_some() || h.plus == Some(None);
    let (git_old, git_new) = h.git_paths.unzip();
    let old = h.renamed.0.or(h.copied.0).or(h.minus.flatten()).or(git_old);
    let new = h.renamed.1.or(h.copied.1).or(h.plus.flatten()).or(git_new);
    let change = match (renamed_or_copied, added, deleted) {
        (Some(change), _, _) => change,
        (None, true, true) => return error(line, "a file can't be both added and deleted"),
        (None, true, false) => Change::Added,
        (None, false, true) => Change::Deleted,
        (None, false, false) => Change::Modified,
    };
    let checked = |path: Option<String>| -> Result<RepoPath> {
        let Some(path) = path else {
            return error(line, "can't tell which file this is");
        };
        RepoPath::parse(&path).or_else(|e| error(line, e.to_string()))
    };
    let (old_path, new_path) = match change {
        Change::Added => (None, Some(checked(new)?)),
        Change::Deleted => (Some(checked(old)?), None),
        _ => (Some(checked(old)?), Some(checked(new)?)),
    };
    if h.old_mode.is_some() != h.new_mode.is_some() {
        return error(line, "`old mode` and `new mode` must come together");
    }
    if change == Change::Modified && old_path != new_path {
        return error(
            line,
            "the old and new paths differ but there is no rename or copy header",
        );
    }
    let (old_mode, new_mode) = match change {
        Change::Added => (None, h.new_file),
        Change::Deleted => (h.deleted_file, None),
        _ => (h.old_mode, h.new_mode),
    };
    Ok(FileDiff {
        old_path,
        new_path,
        change,
        old_mode,
        new_mode,
        binary: h.binary,
        hunks,
    })
}

impl Header {
    fn renamed_or_copied(&self) -> Option<Change> {
        let similarity = self.similarity.unwrap_or(0);
        if self.renamed.0.is_some() || self.renamed.1.is_some() {
            Some(Change::Renamed { similarity })
        } else if self.copied.0.is_some() || self.copied.1.is_some() {
            Some(Change::Copied { similarity })
        } else {
            None
        }
    }
}

/// Reads consecutive hunks starting at `lines[at]`; returns the index after the last.
fn parse_hunks(lines: &[&str], mut at: usize, hunks: &mut Vec<Hunk>) -> Result<usize> {
    while at < lines.len() && lines[at].starts_with("@@ ") {
        let header_line = at + 1;
        let mut hunk = hunk_header(lines[at], header_line)?;
        if let Some(prev) = hunks.last() {
            let after = |h: &Hunk, side| {
                h.first(side)
                    + if side == Side::Old {
                        h.old_lines
                    } else {
                        h.new_lines
                    }
            };
            if hunk.first(Side::Old) < after(prev, Side::Old)
                || hunk.first(Side::New) < after(prev, Side::New)
            {
                return error(header_line, "hunks overlap or are out of order");
            }
        }
        let (mut old_left, mut new_left) = (hunk.old_lines, hunk.new_lines);
        at += 1;
        while old_left > 0 || new_left > 0 {
            let line = lines.get(at).copied();
            let (kind, text) = match line {
                Some(l) if l.starts_with(' ') || l.is_empty() => {
                    (LineKind::Context, l.get(1..).unwrap_or(""))
                }
                Some(l) if l.starts_with('-') => (LineKind::Removed, &l[1..]),
                Some(l) if l.starts_with('+') => (LineKind::Added, &l[1..]),
                Some(l) if l.starts_with('\\') => {
                    mark_missing_newline(&mut hunk, at + 1)?;
                    at += 1;
                    continue;
                }
                _ => {
                    let lines = |n: u32| if n == 1 { "line" } else { "lines" };
                    return error(
                        header_line,
                        format!(
                            "hunk ends early: expected {old_left} more old {} and {new_left} more new {}",
                            lines(old_left),
                            lines(new_left)
                        ),
                    );
                }
            };
            let (needs_old, needs_new) = match kind {
                LineKind::Context => (true, true),
                LineKind::Removed => (true, false),
                LineKind::Added => (false, true),
            };
            if (needs_old && old_left == 0) || (needs_new && new_left == 0) {
                return error(at + 1, "line is outside the hunk's line counts");
            }
            old_left -= u32::from(needs_old);
            new_left -= u32::from(needs_new);
            hunk.lines.push(Line {
                kind,
                text: text.to_string(),
                missing_newline: false,
            });
            at += 1;
        }
        // The marker can follow the hunk's last line.
        if lines.get(at).is_some_and(|l| l.starts_with('\\')) {
            mark_missing_newline(&mut hunk, at + 1)?;
            at += 1;
        }
        hunks.push(hunk);
    }
    Ok(at)
}

fn mark_missing_newline(hunk: &mut Hunk, line: usize) -> Result<()> {
    match hunk.lines.last_mut() {
        Some(last) if !last.missing_newline => {
            last.missing_newline = true;
            Ok(())
        }
        _ => error(
            line,
            "`\\ No newline at end of file` without a line before it",
        ),
    }
}

/// `@@ -os[,ol] +ns[,nl] @@[ section]`.
fn hunk_header(line: &str, n: usize) -> Result<Hunk> {
    let bad = || error(n, format!("invalid hunk header {line:?}"));
    let Some(rest) = line.strip_prefix("@@ -") else {
        return bad();
    };
    let Some((old, rest)) = rest.split_once(" +") else {
        return bad();
    };
    let Some((new, rest)) = rest.split_once(" @@") else {
        return bad();
    };
    let section = match rest {
        "" => "",
        _ => match rest.strip_prefix(' ') {
            Some(section) => section,
            None => return bad(),
        },
    };
    let range = |r: &str| -> Option<(u32, u32)> {
        match r.split_once(',') {
            Some((start, lines)) => Some((start.parse().ok()?, lines.parse().ok()?)),
            None => Some((r.parse().ok()?, 1)),
        }
    };
    let (Some((old_start, old_lines)), Some((new_start, new_lines))) = (range(old), range(new))
    else {
        return bad();
    };
    // Every line number later code computes stays at or below start + lines (start + 1
    // for an empty side), so checking that once here rules out overflow everywhere.
    let fits = |start: u32, lines: u32| start.checked_add(lines.max(1)).is_some();
    if !fits(old_start, old_lines) || !fits(new_start, new_lines) {
        return error(n, format!("hunk range too large in {line:?}"));
    }
    Ok(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        section: section.to_string(),
        lines: Vec::new(),
    })
}

fn mode(text: &str, n: usize) -> Result<Mode> {
    match u32::from_str_radix(text, 8) {
        Ok(value) if text.len() == 6 => Ok(Mode(value)),
        _ => error(n, format!("invalid file mode {text:?}")),
    }
}

fn percent(text: &str, n: usize) -> Result<u8> {
    match text.strip_suffix('%').and_then(|t| t.parse::<u8>().ok()) {
        Some(value) if value <= 100 => Ok(value),
        _ => error(n, format!("invalid similarity {text:?}")),
    }
}

/// The paths on a `diff --git a/old b/new` line, without their prefixes, when they can
/// be told apart. Unquoted paths with spaces are ambiguous unless both are the same, as
/// in git's own parser; the `---`/`+++` or rename lines decide those.
fn git_header_paths(rest: &str, n: usize) -> Result<Option<(String, String)>> {
    let (old, new) = if rest.starts_with('"') {
        let (old, after) = unquote(rest, n)?;
        let Some(after) = after.strip_prefix(' ') else {
            return Ok(None);
        };
        let new = if after.starts_with('"') {
            unquote(after, n)?.0
        } else {
            after.to_string()
        };
        (old, new)
    } else if let Some(split) = rest.find(" \"") {
        (rest[..split].to_string(), unquote(&rest[split + 1..], n)?.0)
    } else {
        // `a/X b/X`: split in the middle and accept only if both halves name X.
        let middle = rest.len() / 2;
        if !rest.is_char_boundary(middle) {
            return Ok(None);
        }
        let (old, new) = rest.split_at(middle);
        match new.strip_prefix(' ') {
            Some(new) if old.strip_prefix("a/").is_some() && old.get(2..) == new.get(2..) => {
                (old.to_string(), new.to_string())
            }
            _ => return Ok(None),
        }
    };
    Ok(old
        .strip_prefix("a/")
        .zip(new.strip_prefix("b/"))
        .map(|(o, nw)| (o.to_string(), nw.to_string())))
}

/// A `---`/`+++` path: `/dev/null` (`None`) or `prefix` + path. Unquoted names with a
/// space get a trailing tab from git, which isn't part of the name.
fn side_path(rest: &str, prefix: &str, n: usize) -> Result<Option<String>> {
    if rest == "/dev/null" {
        return Ok(None);
    }
    let path = if rest.starts_with('"') {
        unquote(rest, n)?.0
    } else {
        rest.split('\t').next().unwrap_or(rest).to_string()
    };
    match path.strip_prefix(prefix) {
        Some(path) => Ok(Some(path.to_string())),
        None => error(
            n,
            format!("expected the path to start with {prefix:?}: {path:?}"),
        ),
    }
}

/// A path on a `rename`/`copy` line: no prefix, maybe quoted.
fn plain_path(rest: &str, n: usize) -> Result<String> {
    if rest.starts_with('"') {
        Ok(unquote(rest, n)?.0)
    } else {
        Ok(rest.to_string())
    }
}

/// Decodes a C-quoted string as git writes it (`core.quotePath`): `"…"` with `\"`,
/// `\\`, `\t`-style escapes and `\ooo` octal bytes. Returns it and the text after.
fn unquote(text: &str, n: usize) -> Result<(String, &str)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 1; // past the opening quote
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let decoded = String::from_utf8(out).or_else(|_| error(n, "path is not UTF-8"))?;
                return Ok((decoded, &text[i + 1..]));
            }
            b'\\' => {
                let escaped = *bytes.get(i + 1).ok_or_else(|| ParseError {
                    line: n,
                    message: "unterminated quoted path".into(),
                })?;
                let simple = match escaped {
                    b'a' => Some(7),
                    b'b' => Some(8),
                    b't' => Some(b'\t'),
                    b'n' => Some(b'\n'),
                    b'v' => Some(11),
                    b'f' => Some(12),
                    b'r' => Some(b'\r'),
                    b'"' => Some(b'"'),
                    b'\\' => Some(b'\\'),
                    _ => None,
                };
                if let Some(byte) = simple {
                    out.push(byte);
                    i += 2;
                } else {
                    let octal = text
                        .get(i + 1..i + 4)
                        .and_then(|o| u8::from_str_radix(o, 8).ok());
                    let Some(byte) = octal else {
                        return error(n, "invalid escape in a quoted path");
                    };
                    out.push(byte);
                    i += 4;
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    error(n, "unterminated quoted path")
}
