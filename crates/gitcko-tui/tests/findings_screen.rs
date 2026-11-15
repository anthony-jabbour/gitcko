// SPDX-License-Identifier: AGPL-3.0-or-later

//! Findings, coverage, copy-fix, and editor intent.

use gitcko_protocol::review::Review;
use gitcko_tui::{App, Effect, KeyCode, cell_fg, key, preview_with_review, render_text};
use ratatui::style::Color;

fn fixture() -> Review {
    serde_json::from_str(include_str!(
        "../../gitcko-protocol/tests/data/review-full.json"
    ))
    .expect("fixture review")
}

fn snap(name: &str, app: &App) {
    for cols in [80u16, 120, 200] {
        let frame = render_text(app, cols, 24);
        insta::assert_snapshot!(format!("{name}_{cols}"), frame);
    }
}

#[test]
fn findings_lists_severity_markers_and_detail() {
    let app = preview_with_review(fixture());
    snap("findings_complete", &app);
    let frame = render_text(&app, 80, 24);
    assert!(frame.contains("[P2]"), "{frame}");
    assert!(frame.contains("src/retry.ts:42"), "{frame}");
    assert!(!frame.contains("No issues found"), "{frame}");
}

#[test]
fn coverage_toggle_and_copy_and_editor_do_not_panic() {
    let mut app = preview_with_review(fixture());
    assert_eq!(app.on_key(key(KeyCode::Char('c'))), Effect::None);
    let coverage = render_text(&app, 120, 24);
    assert!(coverage.contains("changed files: 3"), "{coverage}");
    assert!(coverage.contains("verify: not_run"), "{coverage}");

    let copy = app.on_key(key(KeyCode::Char('y')));
    assert!(matches!(copy, Effect::CopyFix(_)));
    assert!(app.status.contains("copied fix"), "{}", app.status);

    let editor = app.on_key(key(KeyCode::Char('o')));
    assert!(matches!(
        editor,
        Effect::OpenEditor { ref path, line: 42 } if path == "src/retry.ts"
    ));
}

#[test]
fn d_shows_the_hunk_without_graph_keys() {
    let mut app = preview_with_review(fixture());
    assert_eq!(app.on_key(key(KeyCode::Char('d'))), Effect::None);
    let diff = render_text(&app, 120, 24);
    assert!(diff.contains("clearTimeout(timer)"), "{diff}");
    assert!(!diff.contains("gd "), "P5 has no graph jumps");
}

#[test]
fn an_incomplete_findings_screen_is_red_and_not_the_clean_control() {
    let mut partial = gitcko_tui::preview_partial();
    assert_eq!(
        cell_fg(&partial, 120, 24, "partial"),
        Some(Color::Red),
        "partial status is a problem state"
    );
    let clean = preview_with_review(fixture());
    assert_ne!(
        cell_fg(&clean, 120, 24, "complete"),
        Some(Color::Red),
        "complete is the control"
    );
    partial.color = false;
    assert_ne!(
        cell_fg(&partial, 120, 24, "partial"),
        Some(Color::Red),
        "NO_COLOR drops the red"
    );
    let frame = render_text(&partial, 80, 24);
    assert!(frame.contains("status: partial"), "{frame}");
    assert!(!frame.contains("No issues found"), "{frame}");
    snap("findings_partial", &partial);
}
