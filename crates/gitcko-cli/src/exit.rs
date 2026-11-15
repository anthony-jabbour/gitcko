// SPDX-License-Identifier: AGPL-3.0-or-later

//! Exit codes for `review` (CLI_SPEC §5, P5_PLAN §5.1). Pure so the matrix can be
//! tested without a repository. Precedence inside a finished run is
//! `130 > 1 > 7 > 0`. Usage `2` and unavailable `6` are decided before a run starts,
//! so they never reach this function.

use gitcko_protocol::review::{Finding, Lifecycle, ReviewStatus, Severity};

/// Exit code for a finished review. Canceled wins over a findings gate, and a
/// partial or failed review is `1` even when it contains a P0: incompleteness must
/// not look like a successful gate.
pub fn review_exit(status: ReviewStatus, findings: &[Finding], fail_on: Option<Severity>) -> u8 {
    match status {
        ReviewStatus::Canceled => 130,
        ReviewStatus::Partial | ReviewStatus::Failed | ReviewStatus::Superseded => 1,
        ReviewStatus::Complete => {
            if fail_on.is_some_and(|threshold| hits(findings, threshold)) {
                7
            } else {
                0
            }
        }
    }
}

/// Whether an open finding is at least as severe as `threshold`. P0 is above P1.
fn hits(findings: &[Finding], threshold: Severity) -> bool {
    findings.iter().any(|finding| {
        finding.lifecycle != Lifecycle::Resolved && rank(finding.severity) <= rank(threshold)
    })
}

fn rank(severity: Severity) -> u8 {
    match severity {
        Severity::P0 => 0,
        Severity::P1 => 1,
        Severity::P2 => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitcko_protocol::review::Review;

    fn review() -> Review {
        serde_json::from_str(include_str!(
            "../../gitcko-protocol/tests/data/review-full.json"
        ))
        .expect("protocol fixture")
    }

    fn with_status(status: ReviewStatus) -> (ReviewStatus, Vec<Finding>) {
        let mut review = review();
        review.status = status;
        (review.status, review.findings)
    }

    #[test]
    fn complete_clean_is_0_and_a_fail_on_hit_is_7() {
        let mut review = review();
        review.findings.clear();
        assert_eq!(review_exit(review.status, &review.findings, None), 0);
        assert_eq!(
            review_exit(review.status, &review.findings, Some(Severity::P1)),
            0,
            "a complete review with no findings does not trip --fail-on"
        );

        let (_, findings) = with_status(ReviewStatus::Complete);
        assert_eq!(findings[0].severity, Severity::P2);
        assert_eq!(
            review_exit(ReviewStatus::Complete, &findings, Some(Severity::P1)),
            0
        );
        assert_eq!(
            review_exit(ReviewStatus::Complete, &findings, Some(Severity::P2)),
            7
        );
    }

    #[test]
    fn partial_is_1_even_when_fail_on_would_match_a_p0() {
        let mut review = review();
        review.status = ReviewStatus::Partial;
        review.findings[0].severity = Severity::P0;
        assert_eq!(
            review_exit(review.status, &review.findings, Some(Severity::P0)),
            1
        );
        assert_eq!(
            review_exit(ReviewStatus::Failed, &review.findings, Some(Severity::P0)),
            1
        );
        assert_eq!(
            review_exit(ReviewStatus::Canceled, &review.findings, Some(Severity::P0)),
            130,
            "cancel outranks the findings gate"
        );
    }

    #[test]
    fn status_exit_codes_match_the_plan() {
        assert_eq!(status_exit(StatusView::Complete), 0);
        assert_eq!(status_exit(StatusView::Partial), 1);
        assert_eq!(status_exit(StatusView::Missing), 1);
        assert_eq!(status_exit(StatusView::Running), 3);
        assert_eq!(status_exit(StatusView::Failed), 4);
        assert_eq!(status_exit(StatusView::Canceled), 5);
    }
}

/// What `review status` is looking at. Separate from [`review_exit`] because a stored
/// failure is 4 here and 1 on the review command that just produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusView {
    /// A complete stored review.
    Complete,
    /// A partial stored review.
    Partial,
    /// A failed stored review, or a marker whose process is gone.
    Failed,
    /// A canceled stored review.
    Canceled,
    /// A marker whose process is still alive.
    Running,
    /// Nothing stored for this commit.
    Missing,
    /// A newer head replaced the run.
    Superseded,
}

/// Exit code for `review status` (P5_PLAN §5.1). Missing is 1, the same code as a
/// partial review, so a hook does not treat "no review" as a clean commit.
pub fn status_exit(view: StatusView) -> u8 {
    match view {
        StatusView::Complete => 0,
        StatusView::Partial | StatusView::Missing | StatusView::Superseded => 1,
        StatusView::Running => 3,
        StatusView::Failed => 4,
        StatusView::Canceled => 5,
    }
}
