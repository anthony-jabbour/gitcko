// SPDX-License-Identifier: AGPL-3.0-or-later

//! Live progress for one review. Events carry stage names, durations and counts.
//! Candidate text never reaches the screen (P5_PLAN §5).

use std::collections::BTreeMap;

use gitcko_protocol::event::{RunEvent, RunEventKind};
use gitcko_protocol::review::Review;

/// Where the review on this screen is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Running,
    Canceled,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagePhase {
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Stage {
    name: String,
    phase: StagePhase,
    duration_ms: Option<u64>,
    counts: BTreeMap<String, u64>,
}

/// The review pane: stages seen so far, and the stored result once it exists.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewPane {
    pub phase: Phase,
    stages: Vec<Stage>,
    tick: u32,
    model_calls: u32,
    input_tokens: u64,
    output_tokens: u64,
    spend_micros: Option<i64>,
    files_read: u64,
    pub(crate) result: Option<Review>,
}

impl ReviewPane {
    pub fn review(&self) -> Option<&Review> {
        self.result.as_ref()
    }
    pub fn idle() -> ReviewPane {
        ReviewPane {
            phase: Phase::Idle,
            stages: Vec::new(),
            tick: 0,
            model_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            spend_micros: None,
            files_read: 0,
            result: None,
        }
    }

    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn apply(&mut self, event: &RunEvent) {
        match &event.kind {
            RunEventKind::StepStarted { step } => {
                let stage = self.stage(step);
                stage.phase = StagePhase::Running;
                stage.duration_ms = None;
            }
            RunEventKind::StepCompleted {
                step,
                outcome,
                duration_ms,
                counts,
            } => {
                let files = files_from(step, counts);
                let stage = self.stage(step);
                stage.phase = if outcome == "ok" {
                    StagePhase::Done
                } else {
                    StagePhase::Failed
                };
                stage.duration_ms = Some(*duration_ms);
                stage.counts.clone_from(counts);
                if let Some(files) = files {
                    self.files_read = files;
                }
            }
            RunEventKind::ModelCallCompleted {
                input_tokens,
                output_tokens,
                cost_usd,
                ..
            } => {
                self.model_calls = self.model_calls.saturating_add(1);
                if let Some(tokens) = input_tokens {
                    self.input_tokens = self.input_tokens.saturating_add(*tokens);
                }
                if let Some(tokens) = output_tokens {
                    self.output_tokens = self.output_tokens.saturating_add(*tokens);
                }
                if let Some(cost) = cost_usd {
                    add_cost(&mut self.spend_micros, *cost);
                }
            }
            _ => {}
        }
    }

    fn stage(&mut self, name: &str) -> &mut Stage {
        if let Some(index) = self.stages.iter().position(|stage| stage.name == name) {
            return &mut self.stages[index];
        }
        self.stages.push(Stage {
            name: name.to_string(),
            phase: StagePhase::Running,
            duration_ms: None,
            counts: BTreeMap::new(),
        });
        self.stages.last_mut().expect("stage was just pushed")
    }
}

fn files_from(step: &str, counts: &BTreeMap<String, u64>) -> Option<u64> {
    if step == "snapshot" || step == "dispositions" {
        counts.get("paths").copied()
    } else {
        None
    }
}

fn add_cost(total: &mut Option<i64>, cost: f64) {
    if !cost.is_finite() {
        return;
    }
    let micros = (cost * 1_000_000.0).round();
    if !micros.is_finite() {
        return;
    }
    let micros = micros.clamp(i64::MIN as f64, i64::MAX as f64) as i64;
    *total = Some(total.unwrap_or(0).saturating_add(micros));
}

/// Lines for the Review body. `base` is the branch the header already resolved.
pub fn lines(pane: &ReviewPane, base: &str, width: usize, reduced_motion: bool) -> Vec<String> {
    let mut lines = vec!["target".into(), "> branch vs base".into()];
    if pane.phase == Phase::Canceled {
        lines.push("canceled".into());
    }
    if let Some(review) = &pane.result {
        let cols = u16::try_from(width).unwrap_or(80);
        lines.extend(
            gitcko_render::boxed(review, cols)
                .lines()
                .map(str::to_string),
        );
    }
    if busy(pane) {
        let running = pane
            .stages
            .iter()
            .filter(|stage| stage.phase == StagePhase::Running)
            .count();
        for stage in &pane.stages {
            lines.push(stage_line(stage, pane.tick, reduced_motion));
        }
        lines.push(format!("investigators: {running}"));
        lines.push(format!("files: {}", pane.files_read));
        lines.push(format!("model calls: {}", pane.model_calls));
        lines.push(format!(
            "tokens: in {} out {}",
            pane.input_tokens, pane.output_tokens
        ));
        lines.push(spend_line(pane.spend_micros));
    } else if pane.result.is_none() && pane.phase != Phase::Canceled {
        lines.push(format!("Enter reviews this branch against {base}."));
    }
    lines
}

fn busy(pane: &ReviewPane) -> bool {
    pane.phase == Phase::Running || !pane.stages.is_empty() || pane.model_calls > 0
}

fn stage_line(stage: &Stage, tick: u32, reduced_motion: bool) -> String {
    let mark = match stage.phase {
        StagePhase::Running if reduced_motion => "working",
        StagePhase::Running => ["|", "/", "-", "\\"][(tick as usize) % 4],
        StagePhase::Done => "done",
        StagePhase::Failed => "failed",
    };
    let mut line = format!("{mark}  {}", stage.name);
    if let Some(ms) = stage.duration_ms {
        line.push_str(&format!("  {ms}ms"));
    }
    for (key, value) in &stage.counts {
        line.push_str(&format!("  {key} {value}"));
    }
    line
}

fn spend_line(micros: Option<i64>) -> String {
    let Some(micros) = micros else {
        return "spend: not reported".into();
    };
    let sign = if micros < 0 { "-" } else { "" };
    let micros = micros.unsigned_abs();
    let dollars = micros / 1_000_000;
    let frac = micros % 1_000_000;
    format!("spend: {sign}${dollars}.{frac:06}")
}
