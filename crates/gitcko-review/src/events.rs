// SPDX-License-Identifier: AGPL-3.0-or-later

//! The run's event stream (OBSERVABILITY §4): names, codes and numbers only.

use std::collections::BTreeMap;

use gitcko_model::CallRecord;
use gitcko_protocol::event::{PromptRef, RunEvent, RunEventKind};
use tokio::time::Instant;

/// Collects a run's events in order.
pub struct Events<'a> {
    run_id: String,
    now: &'a dyn Fn() -> String,
    events: Vec<RunEvent>,
    step_started: Instant,
}

/// A value's serde spelling, as the contract's other fields use it.
fn wire(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

impl<'a> Events<'a> {
    pub fn new(run_id: &str, now: &'a dyn Fn() -> String) -> Events<'a> {
        Events {
            run_id: run_id.to_string(),
            now,
            events: Vec::new(),
            step_started: Instant::now(),
        }
    }

    fn push(&mut self, kind: RunEventKind) {
        self.events.push(RunEvent {
            run_id: self.run_id.clone(),
            seq: self.events.len() as u64 + 1,
            at: (self.now)(),
            kind,
        });
    }

    pub fn started(&mut self, step: &str) {
        self.step_started = Instant::now();
        self.push(RunEventKind::StepStarted { step: step.into() });
    }

    pub fn completed(&mut self, step: &str, outcome: &str, counts: &[(&str, u64)]) {
        let counts: BTreeMap<String, u64> =
            counts.iter().map(|(k, v)| ((*k).to_string(), *v)).collect();
        self.push(RunEventKind::StepCompleted {
            step: step.into(),
            outcome: outcome.into(),
            duration_ms: u64::try_from(self.step_started.elapsed().as_millis()).unwrap_or(u64::MAX),
            counts,
        });
    }

    pub fn model_call(&mut self, step: &str, record: &CallRecord) {
        self.push(RunEventKind::ModelCallCompleted {
            step: Some(step.into()),
            role: wire(&record.role),
            binding: record.binding.clone(),
            // ponytail: a binding's name is its class until bindings can share a class.
            class: record.binding.clone(),
            provider: wire(&record.provider),
            family: wire(&record.family),
            reasoning_effort: record.reasoning_effort.as_ref().map(wire),
            policy: record.policy.clone(),
            prompt: PromptRef {
                id: record.prompt.id.clone(),
                version: record.prompt.version.clone(),
            },
            attempt: record.attempt,
            fallback_from: record.fallback.as_ref().map(|f| f.from.clone()),
            fallback_reason: record.fallback.as_ref().map(|f| f.reason.to_string()),
            inversion: record.inversion.as_ref().map(wire),
            input_tokens: record.input_tokens,
            output_tokens: record.output_tokens,
            cost_usd: record.cost_usd,
            first_token_ms: record.first_token_ms,
            duration_ms: record.total_ms,
            outcome: wire(&record.outcome),
        });
    }

    pub fn into_vec(self) -> Vec<RunEvent> {
        self.events
    }
}
