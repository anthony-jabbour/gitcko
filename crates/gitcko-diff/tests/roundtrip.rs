// SPDX-License-Identifier: AGPL-3.0-or-later

//! Any change set we can build renders to diff text that parses back to the same value,
//! and line mapping agrees with walking the hunks by hand.

use gitcko_diff::{Change, ChangeSet, FileDiff, Hunk, Line, LineKind, Mode, Side, parse};
use gitcko_domain::RepoPath;
use proptest::prelude::*;

fn path() -> impl Strategy<Value = RepoPath> {
    // Includes spaces, a quote, a backslash and non-ASCII so quoting is exercised.
    "[a-z]{1,6}( [a-z]{1,3}|\"|\\\\|é)?(/[a-z]{1,6}){0,2}"
        .prop_map(|p| RepoPath::parse(&p).unwrap())
}

fn text() -> impl Strategy<Value = String> {
    "[ -~\r]{0,12}"
}

fn kind() -> impl Strategy<Value = LineKind> {
    prop_oneof![
        Just(LineKind::Context),
        Just(LineKind::Added),
        Just(LineKind::Removed)
    ]
}

/// Hunks for a modified file: each has a gap of unchanged lines before it, and at least
/// one added or removed line, so they never touch or overlap.
fn hunks() -> impl Strategy<Value = Vec<Hunk>> {
    let one = (
        1u32..6,
        prop::collection::vec((kind(), text()), 1..8),
        any::<bool>(),
    );
    prop::collection::vec(one, 0..4).prop_map(|specs| {
        let (mut old, mut new) = (1u32, 1u32); // next line number on each side
        let mut hunks = Vec::new();
        for (gap, mut lines, _) in specs {
            if lines.iter().all(|(k, _)| *k == LineKind::Context) {
                lines.push((LineKind::Added, "x".into()));
            }
            old += gap;
            new += gap;
            let old_lines = lines.iter().filter(|(k, _)| *k != LineKind::Added).count() as u32;
            let new_lines = lines
                .iter()
                .filter(|(k, _)| *k != LineKind::Removed)
                .count() as u32;
            hunks.push(Hunk {
                old_start: if old_lines == 0 { old - 1 } else { old },
                old_lines,
                new_start: if new_lines == 0 { new - 1 } else { new },
                new_lines,
                section: String::new(),
                lines: lines
                    .into_iter()
                    .map(|(kind, text)| Line {
                        kind,
                        text,
                        missing_newline: false,
                    })
                    .collect(),
            });
            old += old_lines;
            new += new_lines;
        }
        hunks
    })
}

fn file() -> impl Strategy<Value = FileDiff> {
    let modes = prop_oneof![Just(None), Just(Some((Mode(0o100644), Mode(0o100755))))];
    prop_oneof![
        // Modified, possibly renamed or with a mode change.
        (
            path(),
            path(),
            any::<bool>(),
            0u8..=100,
            modes,
            hunks(),
            any::<bool>()
        )
            .prop_map(|(old, new, renamed, similarity, modes, hunks, binary)| {
                let binary = binary && hunks.is_empty();
                FileDiff {
                    old_path: Some(old.clone()),
                    new_path: Some(if renamed { new } else { old }),
                    change: if renamed {
                        Change::Renamed { similarity }
                    } else {
                        Change::Modified
                    },
                    old_mode: modes.map(|m| m.0),
                    new_mode: modes.map(|m| m.1),
                    binary,
                    hunks,
                }
            }),
        // Added with content.
        (path(), prop::collection::vec(text(), 1..6), any::<bool>()).prop_map(
            |(p, lines, nonl)| {
                let count = lines.len() as u32;
                let mut lines: Vec<Line> = lines
                    .into_iter()
                    .map(|text| Line {
                        kind: LineKind::Added,
                        text,
                        missing_newline: false,
                    })
                    .collect();
                lines.last_mut().unwrap().missing_newline = nonl;
                FileDiff {
                    old_path: None,
                    new_path: Some(p),
                    change: Change::Added,
                    old_mode: None,
                    new_mode: Some(Mode(0o100644)),
                    binary: false,
                    hunks: vec![Hunk {
                        old_start: 0,
                        old_lines: 0,
                        new_start: 1,
                        new_lines: count,
                        section: String::new(),
                        lines,
                    }],
                }
            }
        ),
        // Deleted binary.
        path().prop_map(|p| FileDiff {
            old_path: Some(p),
            new_path: None,
            change: Change::Deleted,
            old_mode: Some(Mode(0o100644)),
            new_mode: None,
            binary: true,
            hunks: vec![],
        }),
    ]
}

/// The new line each old line maps to, by walking every line of the file.
fn walk(file: &FileDiff, old_line: u32) -> Option<u32> {
    let (mut old, mut new) = (1u32, 1u32);
    for hunk in &file.hunks {
        let first_old = if hunk.old_lines == 0 {
            hunk.old_start + 1
        } else {
            hunk.old_start
        };
        while old < first_old {
            if old == old_line {
                return Some(new);
            }
            old += 1;
            new += 1;
        }
        for line in &hunk.lines {
            match line.kind {
                LineKind::Context if old == old_line => return Some(new),
                LineKind::Removed if old == old_line => return None,
                LineKind::Context => (old, new) = (old + 1, new + 1),
                LineKind::Removed => old += 1,
                LineKind::Added => new += 1,
            }
        }
    }
    Some(new + (old_line - old))
}

proptest! {
    #[test]
    fn render_then_parse_gives_the_same_change_set(files in prop::collection::vec(file(), 0..5)) {
        let changes = ChangeSet { files };
        let text = changes.to_string();
        let parsed = parse(&text).map_err(|e| TestCaseError::fail(format!("{e}\n{text}")))?;
        prop_assert_eq!(parsed, changes);
    }

    #[test]
    fn line_mapping_matches_a_line_by_line_walk(hunks in hunks(), probe in 1u32..60) {
        let file = FileDiff {
            old_path: Some(RepoPath::parse("f").unwrap()),
            new_path: Some(RepoPath::parse("f").unwrap()),
            change: Change::Modified,
            old_mode: None,
            new_mode: None,
            binary: false,
            hunks,
        };
        prop_assert_eq!(file.new_line(probe), walk(&file, probe));
        // Going back from where an old line landed returns the same old line.
        if let Some(new) = file.new_line(probe) {
            prop_assert_eq!(file.old_line(new), Some(probe));
            prop_assert!(!file.in_hunk(Side::New, new) || file.in_hunk(Side::Old, probe));
        }
    }
}
