// SPDX-License-Identifier: AGPL-3.0-or-later

//! The hypothesis planner (REVIEW_ENGINE §8): the model's hypotheses and the signals'
//! merged, deduped by a stable key, and ordered by priority then key, never by map order.

use std::collections::BTreeMap;

use gitcko_model::CallError;
use gitcko_prompts::{ChangeContext, PlanOutput, plan as plan_request};
use gitcko_protocol::config::ModelRole;

use crate::MAX_HYPOTHESES;
use crate::complete::{Completeness, reason};
use crate::investigate::{Asked, Calls};
use crate::providers::Signal;
use crate::risk::Risk;
use crate::snapshot::Snapshot;

/// A possible defect to investigate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub path: String,
    pub title: String,
    pub claim: String,
}

impl Task {
    /// What makes two hypotheses the same, however a model words the case and spacing.
    fn key(&self) -> String {
        let title: Vec<String> = self
            .title
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        format!("{}\u{0}{}", self.path, title.join(" "))
    }
}

/// Merges, dedupes and orders. Priority: the risk ranking's position, then unlisted files.
pub fn merge(model: Vec<Task>, signals: &[Signal], risk: &Risk) -> Vec<Task> {
    let seeded = signals.iter().filter(|s| s.seeds_hypothesis).map(|s| Task {
        path: s.path.clone(),
        title: format!("Unchanged callers of {}", s.text),
        claim: format!(
            "Does this change break what its callers rely on? {}",
            s.text
        ),
    });
    let mut unique: BTreeMap<String, Task> = BTreeMap::new();
    for task in model.into_iter().chain(seeded) {
        unique.entry(task.key()).or_insert(task);
    }
    let rank = |t: &Task| {
        risk.priorities
            .iter()
            .position(|p| *p == t.path)
            .unwrap_or(risk.priorities.len())
    };
    let mut tasks: Vec<Task> = unique.into_values().collect();
    // The sort is stable and the map was in key order, so ties fall to the key.
    tasks.sort_by_key(rank);
    tasks.truncate(MAX_HYPOTHESES);
    tasks
}

pub async fn plan(
    calls: &mut Calls<'_, '_>,
    context: &ChangeContext,
    signals: &[Signal],
    risk: &Risk,
    snapshot: &Snapshot<'_>,
    ledger: &mut Completeness,
) -> Vec<Task> {
    let allowed = snapshot.investigated();
    if allowed.is_empty() {
        return Vec::new();
    }
    let model = match calls.ask::<PlanOutput>("plan", plan_request(context)).await {
        Asked::Parsed(output) => output
            .hypotheses
            .into_iter()
            // A hypothesis about anything else, a held-back file included, goes nowhere.
            .filter(|h| allowed.iter().any(|a| a.as_str() == h.path))
            .map(|h| Task {
                path: h.path,
                title: h.title,
                claim: h.claim,
            })
            .collect(),
        Asked::Failed(error) => {
            ledger.call_failed(ModelRole::Review, &error, true);
            return Vec::new();
        }
        Asked::Refused => {
            ledger.call_failed(
                ModelRole::Review,
                &CallError::ProviderFailed(Some(gitcko_model::ProviderError::new(
                    gitcko_model::ErrorKind::ContentPolicy,
                ))),
                true,
            );
            return Vec::new();
        }
        // Signal-seeded hypotheses can still run; the reason stays on the result.
        Asked::Invalid => {
            ledger.add(reason::INVALID_MODEL_OUTPUT);
            Vec::new()
        }
    };
    merge(model, signals, risk)
}

#[cfg(test)]
mod tests {
    use gitcko_protocol::review::{RiskAssessment, RiskLevel, RiskSource};
    use proptest::prelude::*;

    use super::*;

    fn task(path: &str, title: &str) -> Task {
        Task {
            path: path.into(),
            title: title.into(),
            claim: "c".into(),
        }
    }

    fn risk(priorities: &[&str]) -> Risk {
        Risk {
            assessment: RiskAssessment {
                level: RiskLevel::Low,
                source: RiskSource::Rules,
                reason: String::new(),
            },
            priorities: priorities.iter().map(|p| (*p).to_string()).collect(),
        }
    }

    #[test]
    fn duplicates_collapse_across_case_and_spacing_but_not_across_files() {
        let merged = merge(
            vec![
                task("a.rs", "Off by  one"),
                task("a.rs", "off by one"),
                task("b.rs", "off by one"),
            ],
            &[],
            &risk(&[]),
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn the_risk_ranking_comes_first_and_ties_fall_to_the_key() {
        let merged = merge(
            vec![task("a.rs", "z"), task("a.rs", "b"), task("c.rs", "a")],
            &[],
            &risk(&["c.rs"]),
        );
        let order: Vec<(&str, &str)> = merged
            .iter()
            .map(|t| (t.path.as_str(), t.title.as_str()))
            .collect();
        assert_eq!(order, [("c.rs", "a"), ("a.rs", "b"), ("a.rs", "z")]);
    }

    #[test]
    fn only_a_signal_that_seeds_becomes_a_hypothesis() {
        let signal = |seeds| Signal {
            path: "a.rs".into(),
            text: "a.rs: public f changed; 2 caller(s) elsewhere".into(),
            seeds_hypothesis: seeds,
            high_risk: false,
        };
        assert_eq!(merge(vec![], &[signal(true)], &risk(&[])).len(), 1);
        assert!(merge(vec![], &[signal(false)], &risk(&[])).is_empty());
    }

    #[test]
    fn the_plan_is_cut_at_the_cap() {
        let many: Vec<Task> = (0..=MAX_HYPOTHESES)
            .map(|n| task("a.rs", &format!("t{n}")))
            .collect();
        assert_eq!(merge(many, &[], &risk(&[])).len(), MAX_HYPOTHESES);
    }

    proptest! {
        #[test]
        fn the_plan_does_not_depend_on_the_order_the_model_listed_it(
            titles in proptest::collection::btree_set("[a-e]{1,3}", 0..12),
            seed in any::<u64>(),
        ) {
            let forward: Vec<Task> = titles.iter().map(|t| task("a.rs", t)).collect();
            let mut shuffled = forward.clone();
            // A fixed rotation is enough to break any order-dependence.
            if !shuffled.is_empty() {
                let by = (seed as usize) % shuffled.len();
                shuffled.rotate_left(by);
                shuffled.reverse();
            }
            prop_assert_eq!(
                merge(forward, &[], &risk(&[])),
                merge(shuffled, &[], &risk(&[]))
            );
        }
    }
}
