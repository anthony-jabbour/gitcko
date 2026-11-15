// SPDX-License-Identifier: AGPL-3.0-or-later

//! Incomplete reviews must never look clean. Every negative assertion has a control.

use gitcko_protocol::event::{RunEvent, RunEventKind};
use gitcko_protocol::review::{Review, ReviewStatus};
use gitcko_render::{ListItem, boxed, events, list, status_line, text};

const SAMPLE: &str = include_str!("../../gitcko-protocol/tests/data/review-full.json");

fn sample() -> Review {
    serde_json::from_str(SAMPLE).expect("protocol fixture")
}

fn partial_empty() -> Review {
    let mut review = sample();
    review.status = ReviewStatus::Partial;
    review.reason_codes = vec!["budget_exhausted".into(), "deadline".into()];
    review.findings.clear();
    review.score = None;
    review
}

#[test]
fn a_complete_review_with_findings_is_the_clean_control() {
    let rendered = text(&sample());
    assert!(rendered.starts_with("status: complete\n"), "{rendered}");
    assert!(rendered.contains("[P2]"), "{rendered}");
    assert!(
        rendered.contains("Timer is not cleared after the request settles"),
        "{rendered}"
    );
    assert!(!rendered.contains("Review incomplete"), "{rendered}");
}

#[test]
fn a_complete_review_with_no_findings_says_no_issues_found() {
    let mut review = sample();
    review.findings.clear();
    let rendered = text(&review);
    assert!(rendered.contains("No issues found"), "{rendered}");
    assert!(!rendered.contains("Review incomplete"), "{rendered}");
}

#[test]
fn a_partial_review_never_says_no_issues_found() {
    let rendered = text(&partial_empty());
    assert!(
        rendered.starts_with("status: partial (budget_exhausted, deadline)\n"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Review incomplete: nothing was concluded about the changed paths."),
        "{rendered}"
    );
    assert!(!rendered.contains("No issues found"), "{rendered}");
}

#[test]
fn a_partial_review_with_findings_still_leads_with_status() {
    let mut review = sample();
    review.status = ReviewStatus::Partial;
    review.reason_codes = vec!["sensitive_files_held_back".into()];
    review.score = None;
    let rendered = text(&review);
    assert!(
        rendered.starts_with("status: partial (sensitive_files_held_back)\n"),
        "{rendered}"
    );
    assert!(rendered.contains("[P2]"), "{rendered}");
    assert!(!rendered.contains("No issues found"), "{rendered}");
}

#[test]
fn boxed_output_keeps_the_status_line_inside_the_frame() {
    let rendered = boxed(&partial_empty(), 60);
    assert!(rendered.contains("status: partial"), "{rendered}");
    assert!(rendered.starts_with('+'), "{rendered}");
    assert!(!rendered.contains("No issues found"), "{rendered}");
}

#[test]
fn list_rows_are_id_status_and_short_head() {
    let rendered = list(&[ListItem {
        review_id: "rv_01J9ZQ3V7K2M4N6P8R0S2T4V6X".into(),
        status: ReviewStatus::Complete,
        head_sha: "a71e5b0000000000000000000000000000000000".into(),
    }]);
    assert_eq!(
        rendered,
        "rv_01J9ZQ3V7K2M4N6P8R0S2T4V6X  complete  a71e5b0\n"
    );
}

#[test]
fn events_are_a_metadata_timeline() {
    let started = RunEvent {
        run_id: "rv_01J9ZQ3V7K2M4N6P8R0S2T4V6X".into(),
        seq: 1,
        at: "2026-01-01T00:00:00Z".into(),
        kind: RunEventKind::StepStarted {
            step: "snapshot".into(),
        },
    };
    let rendered = events(&[started]);
    assert_eq!(rendered, "1  2026-01-01T00:00:00Z  step.started\n");
    assert!(
        !rendered.contains("snapshot"),
        "the timeline names the event type, not the step text: {rendered}"
    );
}

#[test]
fn an_empty_event_list_is_empty_and_a_second_event_is_the_control() {
    assert_eq!(events(&[]), "");
    let done = RunEvent {
        run_id: "rv_01J9ZQ3V7K2M4N6P8R0S2T4V6X".into(),
        seq: 2,
        at: "2026-01-01T00:00:01Z".into(),
        kind: RunEventKind::StepCompleted {
            step: "snapshot".into(),
            outcome: "ok".into(),
            duration_ms: 1,
            counts: Default::default(),
        },
    };
    let rendered = events(&[done]);
    assert_eq!(rendered, "2  2026-01-01T00:00:01Z  step.completed\n");
}

#[test]
fn failed_and_canceled_status_lines_name_the_status() {
    let mut review = sample();
    review.status = ReviewStatus::Failed;
    review.reason_codes = vec!["review_failed".into()];
    assert_eq!(status_line(&review), "status: failed (review_failed)");
    review.status = ReviewStatus::Canceled;
    review.reason_codes.clear();
    assert_eq!(status_line(&review), "status: canceled");
}
