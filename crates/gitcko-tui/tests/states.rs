// SPDX-License-Identifier: AGPL-3.0-or-later

//! Small terminal, NO_COLOR markers, and scripted review flow keys.

use gitcko_protocol::review::{Review, ReviewStatus};
use gitcko_tui::{App, Effect, KeyCode, Phase, Screen, key, preview_partial, render_text};

fn fixture() -> Review {
    serde_json::from_str(include_str!(
        "../../gitcko-protocol/tests/data/review-full.json"
    ))
    .expect("fixture review")
}

fn snap(name: &str, app: &App, rows: u16) {
    for cols in [80u16, 120, 200] {
        insta::assert_snapshot!(format!("{name}_{cols}"), render_text(app, cols, rows));
    }
}

#[test]
fn a_small_terminal_asks_to_resize_and_any_key_quits() {
    let mut app = App::preview();
    app.cols = 50;
    app.rows = 10;
    assert!(app.too_small());
    let frame = render_text(&app, 50, 10);
    assert!(frame.contains("Terminal too small"), "{frame}");
    assert!(frame.contains("gitcko review --text"), "{frame}");
    snap("small_terminal", &app, 10);
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Effect::Quit);
    assert!(app.quit);
}

#[test]
fn no_color_keeps_p1_markers_on_findings() {
    let mut partial = preview_partial();
    partial.color = false;
    let frame = render_text(&partial, 120, 24);
    assert!(frame.contains("status: partial"), "{frame}");
    assert!(!frame.contains("No issues found"), "{frame}");
    snap("no_color_partial", &partial, 24);
}

#[test]
fn scripted_keys_drive_review_cancel_and_quit() {
    let mut app = App::preview();
    assert_eq!(app.on_key(key(KeyCode::Enter)), Effect::StartReview);
    assert_eq!(app.phase(), Phase::Running);
    assert_eq!(app.on_key(key(KeyCode::Esc)), Effect::CancelReview);
    assert_eq!(app.phase(), Phase::Canceled);
    let mut canceled = fixture();
    canceled.status = ReviewStatus::Canceled;
    canceled.findings.clear();
    app.finish(canceled, false);
    assert_eq!(app.screen, Screen::Review);
    assert_eq!(app.on_key(key(KeyCode::Char('2'))), Effect::None);
    assert_eq!(app.screen, Screen::Findings);
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Effect::Quit);
    assert!(app.quit);
}
