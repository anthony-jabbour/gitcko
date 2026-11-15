// SPDX-License-Identifier: AGPL-3.0-or-later

//! History list and plain code view with line numbers.

use std::fs;

use gitcko_test_support::TempDir;
use gitcko_tui::{
    App, Effect, KeyCode, Screen, key, load_history_fixture, preview_with_review, render_text,
};

fn fixture() -> gitcko_protocol::review::Review {
    serde_json::from_str(include_str!(
        "../../gitcko-protocol/tests/data/review-full.json"
    ))
    .expect("fixture review")
}

fn snap(name: &str, app: &App) {
    for cols in [80u16, 120, 200] {
        insta::assert_snapshot!(format!("{name}_{cols}"), render_text(app, cols, 24));
    }
}

#[test]
fn history_lists_stored_reviews_newest_first() {
    let dir = TempDir::new("tui-history");
    let review = fixture();
    let mut app = App::preview();
    load_history_fixture(&mut app, dir.path(), "repo", &review).expect("stored");
    app.screen = Screen::History;
    snap("history_stored", &app);
    let frame = render_text(&app, 80, 24);
    assert!(frame.contains(&review.review_id), "{frame}");
    assert!(frame.contains("complete"), "{frame}");
}

#[test]
fn enter_on_history_opens_findings() {
    let dir = TempDir::new("tui-history-open");
    let review = fixture();
    let mut app = App::preview();
    load_history_fixture(&mut app, dir.path(), "repo", &review).expect("stored");
    app.screen = Screen::History;
    assert_eq!(app.on_key(key(KeyCode::Enter)), Effect::None);
    assert_eq!(app.screen, Screen::Findings);
    assert!(app.findings.open.is_some());
    let frame = render_text(&app, 120, 24);
    assert!(frame.contains(&review.findings[0].title), "{frame}");
}

#[test]
fn code_view_shows_line_numbers_without_highlighting() {
    let repo = TempDir::new("tui-code");
    let file = repo.path().join("src").join("retry.ts");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "line one\nline two\nline three\n").unwrap();
    let review = fixture();
    let mut app = preview_with_review(review);
    app.repo = Some(gitcko_tui::RepoContext {
        root: repo.path().to_path_buf(),
        cache_id: "repo".into(),
        cache_dir: repo.path().join("cache"),
    });
    app.on_key(key(KeyCode::Enter));
    assert!(app.code.is_some());
    let frame = render_text(&app, 80, 24);
    assert!(frame.contains("retry.ts:42"), "{frame}");
    assert!(frame.contains('|'), "line numbers use a plain bar: {frame}");
    assert!(
        frame.contains("line three"),
        "scroll clamps to the file end: {frame}"
    );

    app.on_key(key(KeyCode::Esc));
    assert!(app.code.is_none());
    let findings = render_text(&app, 80, 24);
    assert!(findings.contains("[P2]"), "{findings}");
}
