// SPDX-License-Identifier: AGPL-3.0-or-later

//! Review: start, live stage counts, cancel. No candidate text.

use gitcko_protocol::event::{PromptRef, RunEvent, RunEventKind};
use gitcko_protocol::review::{Review, ReviewStatus, Severity};
use gitcko_render::status_line;
use gitcko_tui::{App, Effect, KeyCode, KeyEvent, KeyModifiers, Phase, Screen, key, render_text};

fn event(seq: u64, kind: RunEventKind) -> RunEvent {
    RunEvent {
        run_id: "rv_01J9ZQ3V7K2M4N6P8R0S2T4V6X".into(),
        seq,
        at: "2026-01-01T00:00:00Z".into(),
        kind,
    }
}

fn started(step: &str) -> RunEvent {
    event(1, RunEventKind::StepStarted { step: step.into() })
}

fn completed(step: &str, duration_ms: u64, counts: &[(&str, u64)]) -> RunEvent {
    event(
        2,
        RunEventKind::StepCompleted {
            step: step.into(),
            outcome: "ok".into(),
            duration_ms,
            counts: counts
                .iter()
                .map(|(name, value)| ((*name).to_string(), *value))
                .collect(),
        },
    )
}

fn model_call(input: Option<u64>, output: Option<u64>, cost: Option<f64>) -> RunEvent {
    event(
        3,
        RunEventKind::ModelCallCompleted {
            step: Some("investigate".into()),
            role: "review".into(),
            binding: "smart".into(),
            class: "smart".into(),
            provider: "ollama".into(),
            family: "qwen".into(),
            reasoning_effort: None,
            policy: "gitcko-std@1".into(),
            prompt: PromptRef {
                id: "review".into(),
                version: "1".into(),
            },
            attempt: 1,
            fallback_from: None,
            fallback_reason: None,
            inversion: None,
            input_tokens: input,
            output_tokens: output,
            cost_usd: cost,
            first_token_ms: None,
            duration_ms: 10,
            outcome: "ok".into(),
        },
    )
}

fn fixture() -> Review {
    serde_json::from_str(include_str!(
        "../../gitcko-protocol/tests/data/review-full.json"
    ))
    .expect("fixture review")
}

fn ctrl_c() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
}

fn snap(name: &str, app: &App, rows: u16) {
    for cols in [80u16, 120, 200] {
        let frame = render_text(app, cols, rows);
        insta::assert_snapshot!(format!("{name}_{cols}"), frame);
    }
}

#[test]
fn enter_starts_escape_cancels_and_ctrl_c_again_quits() {
    let mut idle = App::preview();
    assert_eq!(idle.on_key(ctrl_c()), Effect::Quit, "an idle ctrl-c quits");
    assert_eq!(idle.phase(), Phase::Idle);
    assert!(!render_text(&App::preview(), 80, 24).contains("canceled"));

    let mut app = App::preview();
    app.screen = Screen::Findings;
    assert_eq!(app.on_key(key(KeyCode::Enter)), Effect::None);
    assert_eq!(app.phase(), Phase::Idle, "Enter on Findings does not start");

    let mut app = App::preview();
    assert_eq!(app.on_key(key(KeyCode::Enter)), Effect::StartReview);
    assert_eq!(app.phase(), Phase::Running);
    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Effect::None,
        "a second Enter does not start another run"
    );
    assert_eq!(app.on_key(key(KeyCode::Esc)), Effect::CancelReview);
    assert_eq!(app.phase(), Phase::Canceled);
    let frame = render_text(&app, 80, 24);
    assert!(frame.contains("canceled"), "{frame}");
    assert!(!frame.contains("cancelled"), "{frame}");
    assert_eq!(app.screen, Screen::Review);
    assert_eq!(app.on_key(ctrl_c()), Effect::Quit);
    assert!(app.quit);
}

#[test]
fn q_asks_before_quitting_a_running_review() {
    let mut app = App::preview();
    app.on_key(key(KeyCode::Enter));
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Effect::None);
    assert!(!app.quit);
    assert!(render_text(&app, 80, 24).contains("Press q again"));
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Effect::QuitAndCancel);
    assert!(app.quit);
    assert_eq!(app.phase(), Phase::Canceled);
}

#[test]
fn progress_is_stage_counts_and_a_running_line_is_the_control() {
    let mut app = App::preview();
    app.on_key(key(KeyCode::Enter));
    app.apply_event(&started("snapshot"));
    let running = render_text(&app, 80, 24);
    assert!(running.contains("|  snapshot"), "{running}");
    assert!(!running.contains("done  snapshot"), "{running}");
    assert!(!running.contains("UNVALIDATED_CANDIDATE"), "{running}");

    app.apply_event(&completed(
        "snapshot",
        12,
        &[("candidates", 4), ("dropped", 1), ("paths", 3)],
    ));
    app.apply_event(&model_call(Some(20), Some(5), None));
    let frame = render_text(&app, 80, 24);
    assert!(
        frame.contains("done  snapshot  12ms  candidates 4  dropped 1  paths 3"),
        "{frame}"
    );
    assert!(frame.contains("files: 3"), "{frame}");
    assert!(frame.contains("investigators: 0"), "{frame}");
    assert!(frame.contains("model calls: 1"), "{frame}");
    assert!(frame.contains("tokens: in 20 out 5"), "{frame}");
    assert!(frame.contains("spend: not reported"), "{frame}");
    assert!(!frame.contains("UNVALIDATED_CANDIDATE"), "{frame}");
    snap("review_running", &app, 24);

    app.apply_event(&model_call(None, None, Some(0.01)));
    let priced = render_text(&app, 80, 24);
    assert!(priced.contains("model calls: 2"), "{priced}");
    assert!(priced.contains("spend: $0.010000"), "{priced}");
}

#[test]
fn reduced_motion_says_working_and_the_spinner_is_the_control() {
    let mut motion = App::preview();
    motion.on_key(key(KeyCode::Enter));
    motion.apply_event(&started("snapshot"));
    let spinning = render_text(&motion, 80, 24);
    assert!(spinning.contains("|  snapshot"), "{spinning}");
    assert!(!spinning.contains("working"), "{spinning}");

    let mut still = App::preview();
    still.reduced_motion = true;
    still.on_key(key(KeyCode::Enter));
    still.apply_event(&started("snapshot"));
    let frame = render_text(&still, 80, 24);
    assert!(frame.contains("working  snapshot"), "{frame}");
    assert!(!frame.contains("|  snapshot"), "{frame}");
}

#[test]
fn a_finished_review_shows_the_cli_box_and_jumps_to_findings() {
    let review = fixture();
    let marker = match review.findings[0].severity {
        Severity::P0 => "[P0]",
        Severity::P1 => "[P1]",
        Severity::P2 => "[P2]",
    };
    let mut app = App::preview();
    app.on_key(key(KeyCode::Enter));
    app.finish(review.clone(), true);
    assert_eq!(app.screen, Screen::Findings);
    assert!(app.status.is_empty());
    app.screen = Screen::Review;
    let frame = render_text(&app, 120, 48);
    assert!(frame.contains(&status_line(&review)), "{frame}");
    assert!(frame.contains(&review.findings[0].title), "{frame}");
    assert!(frame.contains(&review.findings[0].path), "{frame}");
    assert!(frame.contains(marker), "{frame}");
    assert!(!frame.contains("safe to merge"), "{frame}");
    snap("review_finished", &app, 48);

    let mut clean = review.clone();
    clean.findings.clear();
    clean.status = ReviewStatus::Complete;
    let mut app = App::preview();
    app.finish(clean, true);
    app.screen = Screen::Review;
    let frame = render_text(&app, 80, 24);
    assert!(frame.contains("No issues found"), "{frame}");
    assert!(!frame.contains("safe to merge"), "{frame}");
}

#[test]
fn an_incomplete_review_does_not_say_no_issues_were_found() {
    let mut review = fixture();
    review.status = ReviewStatus::Partial;
    review.reason_codes = vec!["budget_exhausted".into()];
    review.findings.clear();
    review.score = None;
    let mut app = App::preview();
    app.on_key(key(KeyCode::Enter));
    app.finish(review, true);
    assert_eq!(app.screen, Screen::Findings);
    app.screen = Screen::Review;
    let frame = render_text(&app, 80, 24);
    assert!(frame.contains("status: partial"), "{frame}");
    assert!(frame.contains("incomplete"), "{frame}");
    assert!(!frame.contains("No issues found"), "{frame}");
    assert!(!frame.contains("safe to merge"), "{frame}");
}

#[test]
fn cancel_keeps_the_review_screen_and_stores_the_canceled_label() {
    let mut review = fixture();
    review.status = ReviewStatus::Canceled;
    review.findings.clear();
    review.score = None;
    let mut app = App::preview();
    app.on_key(key(KeyCode::Enter));
    app.apply_event(&started("snapshot"));
    assert_eq!(app.on_key(key(KeyCode::Esc)), Effect::CancelReview);
    app.finish(review, true);
    assert_eq!(app.screen, Screen::Review);
    assert_eq!(app.phase(), Phase::Canceled);
    assert_eq!(app.status, "canceled");
    let frame = render_text(&app, 80, 40);
    assert!(frame.contains("canceled"), "{frame}");
    assert!(!frame.contains("cancelled"), "{frame}");
    snap("review_canceled", &app, 40);
}

#[test]
fn a_missing_model_or_repository_does_not_start() {
    let mut app = App::preview();
    app.can_review = false;
    app.status = "No model configured. Run `gitcko setup`.".into();
    assert_eq!(app.on_key(key(KeyCode::Enter)), Effect::None);
    assert_eq!(app.phase(), Phase::Idle);
    assert!(render_text(&app, 80, 24).contains("gitcko setup"));

    let mut app = App::preview();
    app.no_repo = Some("/work/notes".into());
    assert_eq!(app.on_key(key(KeyCode::Enter)), Effect::None);
    assert_eq!(app.phase(), Phase::Idle);
}

#[test]
fn a_store_failure_is_named_and_a_stored_run_is_not() {
    let review = fixture();
    let mut stored = App::preview();
    stored.on_key(key(KeyCode::Enter));
    stored.finish(review.clone(), true);
    assert!(stored.status.is_empty());
    assert_eq!(stored.screen, Screen::Findings);

    let mut missed = App::preview();
    missed.on_key(key(KeyCode::Enter));
    missed.finish(review, false);
    assert_eq!(missed.status, "could not store the review");
    assert_eq!(missed.screen, Screen::Findings);
}

#[test]
fn an_unknown_event_adds_no_stage() {
    let mut app = App::preview();
    app.on_key(key(KeyCode::Enter));
    app.apply_event(&event(
        1,
        RunEventKind::LifecycleChanged {
            from: gitcko_protocol::event::RunLifecycle::Queued,
            to: gitcko_protocol::event::RunLifecycle::ReviewingFiles,
        },
    ));
    let frame = render_text(&app, 80, 24);
    assert!(!frame.contains("queued"), "{frame}");
    assert!(
        frame.contains("investigators: 0"),
        "the run is up, with no stage yet: {frame}"
    );
    app.apply_event(&started("snapshot"));
    assert!(render_text(&app, 80, 24).contains("snapshot"));
}
