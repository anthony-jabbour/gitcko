// SPDX-License-Identifier: AGPL-3.0-or-later

//! Writes a [`ChangeSet`] back as git-style unified diff text that [`crate::parse`]
//! reads back to the same value. Blob ids (`index` lines) aren't kept, so they aren't
//! written.

use std::fmt::{self, Display, Formatter, Write};

use gitcko_domain::RepoPath;

use crate::{Change, ChangeSet, FileDiff, LineKind};

impl Display for ChangeSet {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.files.iter().try_for_each(|file| file.fmt(f))
    }
}

impl Display for FileDiff {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let old = self.old_path.as_ref().or(self.new_path.as_ref());
        let new = self.new_path.as_ref().or(self.old_path.as_ref());
        let (Some(old), Some(new)) = (old, new) else {
            return Err(fmt::Error); // a file diff always has at least one path
        };
        writeln!(f, "diff --git {} {}", quoted("a/", old), quoted("b/", new))?;
        match (self.change, self.old_mode, self.new_mode) {
            (Change::Added, _, Some(mode)) => writeln!(f, "new file mode {:06o}", mode.0)?,
            (Change::Deleted, Some(mode), _) => writeln!(f, "deleted file mode {:06o}", mode.0)?,
            (_, Some(old_mode), Some(new_mode)) => writeln!(
                f,
                "old mode {:06o}\nnew mode {:06o}",
                old_mode.0, new_mode.0
            )?,
            _ => {}
        }
        let (verb, similarity) = match self.change {
            Change::Renamed { similarity } => ("rename", similarity),
            Change::Copied { similarity } => ("copy", similarity),
            _ => ("", 0),
        };
        if !verb.is_empty() {
            writeln!(f, "similarity index {similarity}%")?;
            writeln!(f, "{verb} from {}", quoted("", old))?;
            writeln!(f, "{verb} to {}", quoted("", new))?;
        }
        let old_side = match self
            .old_path
            .as_ref()
            .filter(|_| self.change != Change::Added)
        {
            Some(path) => quoted("a/", path),
            None => "/dev/null".to_string(),
        };
        let new_side = match self
            .new_path
            .as_ref()
            .filter(|_| self.change != Change::Deleted)
        {
            Some(path) => quoted("b/", path),
            None => "/dev/null".to_string(),
        };
        if self.binary {
            writeln!(f, "Binary files {old_side} and {new_side} differ")?;
        } else if !self.hunks.is_empty() {
            writeln!(f, "--- {old_side}\n+++ {new_side}")?;
        }
        for hunk in &self.hunks {
            write!(
                f,
                "@@ -{} +{} @@",
                range(hunk.old_start, hunk.old_lines),
                range(hunk.new_start, hunk.new_lines)
            )?;
            if !hunk.section.is_empty() {
                write!(f, " {}", hunk.section)?;
            }
            f.write_char('\n')?;
            for line in &hunk.lines {
                let marker = match line.kind {
                    LineKind::Context => ' ',
                    LineKind::Added => '+',
                    LineKind::Removed => '-',
                };
                writeln!(f, "{marker}{}", line.text)?;
                if line.missing_newline {
                    writeln!(f, "\\ No newline at end of file")?;
                }
            }
        }
        Ok(())
    }
}

/// `start,lines`, or just `start` when there is one line, as git prints it.
fn range(start: u32, lines: u32) -> String {
    if lines == 1 {
        start.to_string()
    } else {
        format!("{start},{lines}")
    }
}

/// `prefix` + path, C-quoted the way git does (`core.quotePath`) when the path holds a
/// quote, a backslash or a non-ASCII byte. `RepoPath` already rules out control bytes.
fn quoted(prefix: &str, path: &RepoPath) -> String {
    let path = path.as_str();
    if !path.bytes().any(|b| b == b'"' || b == b'\\' || b >= 0x80) {
        return format!("{prefix}{path}");
    }
    let mut out = String::from("\"");
    out.push_str(prefix);
    for byte in path.bytes() {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x80.. => write!(out, "\\{byte:03o}").expect("writing to a String"),
            _ => out.push(byte as char),
        }
    }
    out.push('"');
    out
}
