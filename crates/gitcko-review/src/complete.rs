// SPDX-License-Identifier: AGPL-3.0-or-later

//! Completeness (REVIEW_ENGINE §18): a run is `complete` only when no reason code
//! applies. An incomplete run must never look clean, so every gap lands here and the
//! status is derived from what landed, never set by hand.

use gitcko_model::{CallError, ErrorKind, Limit};
use gitcko_protocol::config::ModelRole;
use gitcko_protocol::review::ReviewStatus;

/// The reason codes a result may carry (the phase plan, §10.3).
pub mod reason {
    pub const PROVIDER_FAILURE: &str = "provider_failure";
    pub const REVIEW_FAILED: &str = "review_failed";
    pub const JUDGE_FAILED: &str = "judge_failed";
    pub const MODEL_REFUSED: &str = "model_refused";
    pub const BUDGET_EXHAUSTED: &str = "budget_exhausted";
    pub const TOKENS: &str = "tokens";
    pub const COST: &str = "cost";
    pub const DEADLINE: &str = "deadline";
    pub const REDUCED_CONTEXT: &str = "reduced_context";
    pub const INDEX_GAP: &str = "index_gap";
    pub const PARSE_FAILED: &str = "parse_failed";
    pub const UNSUPPORTED_LANGUAGE: &str = "unsupported_language";
    pub const TRUNCATED: &str = "truncated";
    pub const SENSITIVE_HELD_BACK: &str = "sensitive_files_held_back";
    pub const CONFIG_INVALID: &str = "config_invalid";
    pub const INVALID_MODEL_OUTPUT: &str = "invalid_model_output";
    pub const CANCELED: &str = "canceled";
    /// The repository couldn't be read at all.
    pub const SNAPSHOT_FAILED: &str = "snapshot_failed";
}

use reason::*;

/// Primary order: provider failure, then budget, then reduced context, each followed by
/// its detail. Anything else follows.
const ORDER: [&str; 17] = [
    CANCELED,
    SNAPSHOT_FAILED,
    PROVIDER_FAILURE,
    REVIEW_FAILED,
    JUDGE_FAILED,
    MODEL_REFUSED,
    INVALID_MODEL_OUTPUT,
    BUDGET_EXHAUSTED,
    TOKENS,
    COST,
    DEADLINE,
    REDUCED_CONTEXT,
    INDEX_GAP,
    PARSE_FAILED,
    UNSUPPORTED_LANGUAGE,
    TRUNCATED,
    SENSITIVE_HELD_BACK,
];

/// Everything found to be missing so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completeness {
    reasons: Vec<&'static str>,
    canceled: bool,
    /// Nothing authoritative can come of this run.
    failed: bool,
    /// No further model call should be made.
    stopped: bool,
}

impl Completeness {
    pub fn add(&mut self, code: &'static str) {
        if !self.reasons.contains(&code) {
            self.reasons.push(code);
        }
    }

    /// A reason with its parent, `reduced_context` + `truncated`.
    pub fn reduced(&mut self, detail: &'static str) {
        self.add(REDUCED_CONTEXT);
        self.add(detail);
    }

    /// Records a failure that leaves no authoritative result.
    pub fn fail(&mut self, code: &'static str) {
        self.add(code);
        self.failed = true;
        self.stopped = true;
    }

    /// True once the run should make no more model calls.
    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Records a failed call of a required role. `planning`: nothing was investigated, so
    /// a provider failure here leaves no result at all.
    pub fn call_failed(&mut self, role: ModelRole, error: &CallError, planning: bool) {
        match error {
            CallError::Canceled => {
                self.add(CANCELED);
                self.canceled = true;
                self.stopped = true;
            }
            CallError::BudgetExhausted(limit) => {
                self.add(BUDGET_EXHAUSTED);
                self.add(match limit {
                    Limit::InputTokens | Limit::OutputTokens => TOKENS,
                    Limit::Cost => COST,
                    Limit::Deadline => DEADLINE,
                });
                self.stopped = true;
            }
            CallError::ProviderFailed(Some(e)) if e.kind == ErrorKind::ContentPolicy => {
                self.add(MODEL_REFUSED);
                self.stopped = true;
            }
            CallError::NotConfigured | CallError::ProviderFailed(_) | CallError::Unavailable(_) => {
                self.add(PROVIDER_FAILURE);
                self.add(if role == ModelRole::Judge {
                    JUDGE_FAILED
                } else {
                    REVIEW_FAILED
                });
                self.stopped = true;
                self.failed |= planning;
            }
        }
    }

    /// The status these gaps add up to. The reasons don't choose it by name, so a new
    /// reason can't fall through to `complete`.
    pub fn status(&self) -> ReviewStatus {
        if self.canceled {
            ReviewStatus::Canceled
        } else if self.failed {
            ReviewStatus::Failed
        } else if self.reasons.is_empty() {
            ReviewStatus::Complete
        } else {
            ReviewStatus::Partial
        }
    }

    /// The reason codes in primary order, the same however they were found.
    pub fn reason_codes(&self) -> Vec<String> {
        let rank = |code: &&str| ORDER.iter().position(|o| o == code).unwrap_or(ORDER.len());
        let mut codes = self.reasons.clone();
        codes.sort_by_key(|code| (rank(code), *code));
        codes.into_iter().map(str::to_string).collect()
    }
}

#[cfg(test)]
mod tests {
    use gitcko_model::ProviderError;
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn nothing_missing_is_the_only_way_to_be_complete() {
        assert_eq!(Completeness::default().status(), ReviewStatus::Complete);
        for code in ORDER {
            let mut ledger = Completeness::default();
            ledger.add(code);
            assert_ne!(ledger.status(), ReviewStatus::Complete, "{code}");
        }
        // Not even a code nobody has listed yet.
        let mut ledger = Completeness::default();
        ledger.add("not_in_the_list");
        assert_eq!(ledger.status(), ReviewStatus::Partial);
    }

    #[test]
    fn a_failed_required_role_names_the_role() {
        let down = CallError::ProviderFailed(Some(ProviderError::new(ErrorKind::Server)));
        let mut review = Completeness::default();
        review.call_failed(ModelRole::Review, &down, false);
        assert_eq!(review.reason_codes(), [PROVIDER_FAILURE, REVIEW_FAILED]);
        assert_eq!(review.status(), ReviewStatus::Partial);
        let mut judge = Completeness::default();
        judge.call_failed(ModelRole::Judge, &down, false);
        assert_eq!(judge.reason_codes(), [PROVIDER_FAILURE, JUDGE_FAILED]);
    }

    #[test]
    fn each_limit_is_named() {
        for (limit, code) in [
            (Limit::InputTokens, TOKENS),
            (Limit::OutputTokens, TOKENS),
            (Limit::Cost, COST),
            (Limit::Deadline, DEADLINE),
        ] {
            let mut ledger = Completeness::default();
            ledger.call_failed(ModelRole::Review, &CallError::BudgetExhausted(limit), false);
            assert_eq!(ledger.reason_codes(), [BUDGET_EXHAUSTED, code]);
        }
    }

    #[test]
    fn canceled_wins_over_a_failure_found_earlier() {
        let mut ledger = Completeness::default();
        ledger.fail(SNAPSHOT_FAILED);
        ledger.call_failed(ModelRole::Review, &CallError::Canceled, false);
        assert_eq!(ledger.status(), ReviewStatus::Canceled);
        assert_eq!(ledger.reason_codes()[0], CANCELED);
    }

    #[test]
    fn reduced_context_comes_with_its_detail() {
        let mut ledger = Completeness::default();
        ledger.reduced(TRUNCATED);
        assert_eq!(ledger.reason_codes(), [REDUCED_CONTEXT, TRUNCATED]);
    }

    proptest! {
        #[test]
        fn the_codes_read_the_same_however_they_were_found(
            mut picks in proptest::collection::vec(0..ORDER.len(), 0..20),
        ) {
            let collect = |picks: &[usize]| {
                let mut ledger = Completeness::default();
                for pick in picks {
                    ledger.add(ORDER[*pick]);
                }
                ledger.reason_codes()
            };
            let forward = collect(&picks);
            picks.reverse();
            prop_assert_eq!(forward.clone(), collect(&picks));
            let mut unique = forward.clone();
            unique.dedup();
            prop_assert_eq!(forward, unique);
        }
    }
}
