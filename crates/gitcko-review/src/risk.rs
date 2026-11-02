// SPDX-License-Identifier: AGPL-3.0-or-later

//! Risk classification: `risk-classifier` is optional, so when it fails for any reason
//! but the run stopping, the deterministic signals decide and the result says so.

use gitcko_prompts::{ChangeContext, RiskLevel as ModelLevel, RiskOutput, risk};
use gitcko_protocol::review::{RiskAssessment, RiskLevel, RiskSource};

use crate::investigate::{Asked, Calls};
use crate::providers::Signal;
use crate::snapshot::Snapshot;

/// The assessment plus the model's file priorities, most important first.
pub struct Risk {
    pub assessment: RiskAssessment,
    pub priorities: Vec<String>,
}

fn by_rules(signals: &[Signal]) -> Risk {
    let level = if signals.iter().any(|s| s.high_risk) {
        RiskLevel::High
    } else if signals.iter().any(|s| s.seeds_hypothesis) {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };
    let mut priorities: Vec<String> = Vec::new();
    for signal in signals
        .iter()
        .filter(|s| s.high_risk)
        .chain(signals.iter().filter(|s| !s.high_risk))
    {
        if !priorities.contains(&signal.path) {
            priorities.push(signal.path.clone());
        }
    }
    Risk {
        assessment: RiskAssessment {
            level,
            source: RiskSource::Rules,
            reason: format!("{} risk signal(s) in the change", signals.len()),
        },
        priorities,
    }
}

pub async fn classify(
    calls: &mut Calls<'_, '_>,
    context: &ChangeContext,
    signals: &[Signal],
    snapshot: &Snapshot<'_>,
) -> Risk {
    if snapshot.investigated().is_empty() {
        return by_rules(signals);
    }
    match calls.ask::<RiskOutput>("risk", risk(context)).await {
        Asked::Parsed(output) => {
            let allowed = snapshot.investigated();
            Risk {
                assessment: RiskAssessment {
                    level: match output.level {
                        ModelLevel::Low => RiskLevel::Low,
                        ModelLevel::Medium => RiskLevel::Medium,
                        ModelLevel::High => RiskLevel::High,
                    },
                    source: RiskSource::Model,
                    reason: output.reason,
                },
                priorities: output
                    .priorities
                    .into_iter()
                    .filter(|p| allowed.iter().any(|a| a.as_str() == p))
                    .collect(),
            }
        }
        // Optional. If the run itself stopped, the next call says so.
        Asked::Failed(_) | Asked::Invalid | Asked::Refused => by_rules(signals),
    }
}
