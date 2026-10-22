// SPDX-License-Identifier: AGPL-3.0-or-later

//! The fixtures in `tests/data/` are real `git diff` output (see the README there), so
//! the parser is checked against what git actually prints.

use std::fmt::Write as _;
use std::path::Path;

use gitcko_diff::{Change, ChangeSet, LineKind, Side, parse};

fn fixtures() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut found: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "diff"))
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    found.sort();
    assert!(found.len() >= 2, "fixtures missing from {}", dir.display());
    found
}

fn fixture(name: &str) -> String {
    fixtures().into_iter().find(|(n, _)| n == name).unwrap().1
}

#[test]
fn fixtures_parse_to_their_golden_structure() {
    for (name, text) in fixtures() {
        let changes = parse(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        insta::assert_debug_snapshot!(name.clone(), changes);
    }
}

#[test]
fn fixtures_line_mapping_golden() {
    // For every file with hunks: each new-side line, the old line it maps to (or `+` for
    // an added line), and whether it sits inside a hunk (where an inline comment can go).
    for (name, text) in fixtures() {
        let changes = parse(&text).unwrap();
        let mut table = String::new();
        for file in changes.files.iter().filter(|f| !f.hunks.is_empty()) {
            let shown = file.new_path.as_ref().or(file.old_path.as_ref()).unwrap();
            writeln!(table, "{shown}").unwrap();
            let last = file
                .hunks
                .iter()
                .map(|h| h.new_start + h.new_lines)
                .max()
                .unwrap();
            for new in 1..=last + 1 {
                let old = file
                    .old_line(new)
                    .map_or("+".to_string(), |o| o.to_string());
                let mark = if file.in_hunk(Side::New, new) {
                    "hunk"
                } else {
                    ""
                };
                writeln!(table, "  new {new:>3} -> old {old:>3}  {mark}").unwrap();
            }
        }
        insta::assert_snapshot!(format!("{name}-line-map"), table);
    }
}

#[test]
fn fixtures_survive_render_and_parse() {
    for (name, text) in fixtures() {
        let changes = parse(&text).unwrap();
        let again = parse(&changes.to_string()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(again, changes, "{name}");
    }
}

#[test]
fn empty_diff_is_an_empty_change_set() {
    assert_eq!(parse("").unwrap(), ChangeSet::default());
    assert_eq!(ChangeSet::default().to_string(), "");
}

#[test]
fn quoted_and_spaced_paths_are_decoded() {
    let changes = parse(&fixture("git_many_cases")).unwrap();
    let paths: Vec<_> = changes
        .files
        .iter()
        .filter_map(|f| f.new_path.as_ref())
        .map(|p| p.as_str())
        .collect();
    assert!(paths.contains(&"ünïcode.txt"), "{paths:?}");
    assert!(paths.contains(&"with space.txt"), "{paths:?}");
}

#[test]
fn crlf_lines_keep_their_carriage_return() {
    // Built here: `.gitattributes` would turn a committed CRLF fixture into LF.
    let text = "diff --git a/w.txt b/w.txt\n--- a/w.txt\n+++ b/w.txt\n@@ -1,2 +1,2 @@\n keep\r\n-old\r\n+new\r\n";
    let changes = parse(text).unwrap();
    let lines = &changes.files[0].hunks[0].lines;
    assert_eq!(lines[0].text, "keep\r");
    assert_eq!(lines[2].kind, LineKind::Added);
    assert_eq!(changes.to_string(), text);
}

#[test]
fn huge_hunk_parses_and_maps() {
    let n = 50_000;
    let mut text = format!(
        "diff --git a/big.txt b/big.txt\nnew file mode 100644\n--- /dev/null\n+++ b/big.txt\n@@ -0,0 +1,{n} @@\n"
    );
    for i in 0..n {
        writeln!(text, "+line {i}").unwrap();
    }
    let changes = parse(&text).unwrap();
    let file = &changes.files[0];
    assert_eq!(file.change, Change::Added);
    assert_eq!(file.hunks[0].lines.len(), n as usize);
    assert_eq!(file.old_line(n), None);
    assert!(file.in_hunk(Side::New, n) && !file.in_hunk(Side::New, n + 1));
}

#[test]
fn malformed_diffs_are_rejected_with_the_line_number() {
    let head = "diff --git a/f b/f\n--- a/f\n+++ b/f\n";
    for (text, line, mentions) in [
        (
            format!("{head}@@ -1,3 +1,3 @@\n a\n-b\n+c\n"),
            4,
            "1 more old line",
        ),
        (format!("{head}@@ -1 +1 @@\n a\n b\n"), 6, "outside"),
        (format!("{head}@@ -x +1 @@\n a\n"), 4, "hunk header"),
        (
            format!("{head}@@ -5,1 +5,1 @@\n a\n@@ -1,1 +1,1 @@\n b\n"),
            6,
            "order",
        ),
        ("@@ -1 +1 @@\n a\n".to_string(), 1, "outside a `diff --git`"),
        (
            "diff --git a/f b/f\n--- a/f\n@@ -1 +1 @@\n a\n".to_string(),
            3,
            "+++",
        ),
        // Found by the fuzzer: shapes git never prints, which can't round-trip.
        (
            "diff --git a/f b/f\n--- a/f\n+++ b/g\n@@ -1 +1 @@\n-a\n+b\n".to_string(),
            1,
            "rename",
        ),
        (
            "diff --git a/f b/f\n--- a/f\n+++ b/f\ndiff --git a/g b/g\n".to_string(),
            2,
            "no hunk",
        ),
        (
            "diff --git a/f b/f\nnew mode 100755\n".to_string(),
            1,
            "together",
        ),
    ] {
        let err = parse(&text).unwrap_err();
        assert_eq!(err.line, line, "{err} in\n{text}");
        assert!(
            err.to_string().contains(mentions),
            "{err} should mention {mentions:?}"
        );
    }
}

#[test]
fn paths_escaping_the_repository_are_rejected() {
    // An uploaded patch is untrusted: it can't name a file outside the repo or in .git.
    for path in ["../../etc/passwd", ".git/hooks/pre-commit", "a/../../x"] {
        let text = format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-a\n+b\n"
        );
        let err = parse(&text).unwrap_err();
        assert!(
            err.to_string().contains("invalid repository path"),
            "{path}: {err}"
        );
    }
}

#[test]
fn no_newline_marker_applies_to_the_line_before_it() {
    let changes = parse(&fixture("git_many_cases")).unwrap();
    let file = changes
        .files
        .iter()
        .find(|f| {
            f.new_path
                .as_ref()
                .is_some_and(|p| p.as_str() == "nonl.txt")
        })
        .unwrap();
    let lines = &file.hunks[0].lines;
    let removed = lines.iter().find(|l| l.kind == LineKind::Removed).unwrap();
    let added = lines.iter().find(|l| l.kind == LineKind::Added).unwrap();
    assert!(removed.missing_newline && !added.missing_newline);
}

#[test]
fn non_ascii_header_split_does_not_panic() {
    // Found by the fuzzer: the middle of this `diff --git` line falls inside `ɜ`.
    let result = parse("diff --git a/cop9y_src.txt bɜopy_fy, sin differ\n");
    assert!(result.is_err(), "{result:?}");
}

#[test]
fn hunk_ranges_past_the_largest_line_number_are_rejected() {
    // Untrusted input: `start + lines` must fit, or later arithmetic would overflow.
    for header in [
        "@@ -4294967295,1 +1 @@",
        "@@ -1 +4294967295,2 @@",
        "@@ -4294967295,0 +1 @@",
    ] {
        let text = format!("diff --git a/f b/f\n--- a/f\n+++ b/f\n{header}\n-a\n+b\n");
        let err = parse(&text).unwrap_err();
        assert!(err.to_string().contains("too large"), "{header}: {err}");
    }
}

#[test]
fn format_patch_mail_and_signature_are_skipped() {
    // Default `git format-patch` output: mail headers and a diffstat before, and a
    // `-- ` signature with the git version after.
    let changes = parse(&fixture("git_format_patch")).unwrap();
    assert_eq!(changes.files.len(), 2);
    assert_eq!(changes.files[1].change, Change::Added);
}

#[test]
fn binary_file_whose_name_contains_and_dev_null_stays_modified() {
    let name = "bar and /dev/null";
    let text = format!(
        "diff --git a/{name} b/{name}\nindex 1..2 100644\nBinary files a/{name} and b/{name} differ\n"
    );
    let changes = parse(&text).unwrap();
    assert_eq!(changes.files[0].change, Change::Modified);
    assert_eq!(changes.files[0].new_path.as_ref().unwrap().as_str(), name);
    assert_eq!(parse(&changes.to_string()).unwrap(), changes);
}

#[test]
fn hunks_must_agree_on_unchanged_lines_between_them() {
    // Two unchanged lines between the hunks on the old side but one on the new side
    // would give conflicting line mappings.
    let text = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n a\n@@ -4 +3 @@\n d\n";
    let err = parse(text).unwrap_err();
    assert_eq!(err.line, 6, "{err}");
    assert!(err.to_string().contains("unchanged"), "{err}");
}

#[test]
fn every_path_in_a_header_is_checked_even_unused_ones() {
    let text =
        "diff --git a/../../etc/passwd b/../../etc/passwd\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n";
    let err = parse(text).unwrap_err();
    assert!(err.to_string().contains("invalid repository path"), "{err}");
}

#[test]
fn patch_text_hidden_in_skipped_parts_is_rejected() {
    // `git apply` also takes a plain `---`/`+++`/`@@` patch without a `diff --git`
    // line, so patch-shaped text in the parts the parser skips (before the first file,
    // after a format-patch signature) could be applied without being reviewed.
    let hidden = "--- a/evil\n+++ b/evil\n@@ -1 +1 @@\n-a\n+b\n";
    let real = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n";
    for text in [
        format!("Subject: x\n\n{hidden}{real}"),
        format!("{real}-- \n2.55.0\n\n{hidden}"),
        format!("{real}-- \n2.55.0\nIndex: evil\n"),
    ] {
        let err = parse(&text).unwrap_err();
        assert!(
            err.to_string().contains("outside a `diff --git`"),
            "{err}\n{text}"
        );
    }
}
