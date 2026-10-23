// SPDX-License-Identifier: Apache-2.0

//! Run events: the append-only stream behind the dashboard's live view and
//! `review show --events` (OBSERVABILITY §4).
//!
//! Events carry counts, ids and timings only, never source, finding text or prompts.
//! Billing events (`credits.*`) are Cloud-only and not defined here; a public reader
//! sees them, like any type it doesn't know, as [`RunEventKind::Unknown`].

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One event in a run's stream. New event types can appear without a version bump, so
/// readers must accept a `type` they don't know.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunEvent {
    pub run_id: String,
    /// Position in the run's stream, starting at 1.
    pub seq: u64,
    /// RFC 3339 timestamp in UTC.
    #[schemars(extend("format" = "date-time"))]
    pub at: String,
    #[serde(flatten)]
    pub kind: RunEventKind,
}

/// What happened, tagged by `type` on the wire (OBSERVABILITY §4). Readers keep types
/// they don't know as `Unknown`, so adding an event type later breaks no one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum RunEventKind {
    #[serde(rename = "lifecycle.changed")]
    LifecycleChanged {
        from: RunLifecycle,
        to: RunLifecycle,
    },
    #[serde(rename = "step.started")]
    StepStarted { step: String },
    #[serde(rename = "step.completed")]
    StepCompleted {
        step: String,
        outcome: String,
        duration_ms: u64,
        /// Bounded counts such as files, symbols or hypotheses.
        #[serde(default)]
        counts: BTreeMap<String, u64>,
    },
    #[serde(rename = "model_call.completed")]
    ModelCallCompleted {
        step: Option<String>,
        role: String,
        binding: String,
        class: String,
        duration_ms: u64,
        input_tokens: u64,
        output_tokens: u64,
        /// `null` when the provider doesn't report cost.
        cost_usd: Option<f64>,
        attempt: u32,
        outcome: String,
        /// The binding this call fell back from, if it was a fallback.
        fallback_from: Option<String>,
    },
    #[serde(rename = "breaker.changed")]
    BreakerChanged {
        binding: String,
        state: BreakerState,
    },
    #[serde(rename = "budget.warning")]
    BudgetWarning {
        limit: BudgetLimit,
        used: f64,
        cap: f64,
    },
    #[serde(rename = "budget.exhausted")]
    BudgetExhausted {
        limit: BudgetLimit,
        used: f64,
        cap: f64,
    },
    #[serde(rename = "publish.completed")]
    PublishCompleted {
        target: PublishTarget,
        outcome: String,
        /// True when publishing was refused because a newer head exists.
        stale_rejected: bool,
    },
    /// Any event type this version doesn't define; its fields are dropped. A reader-side
    /// fallback only, so it isn't in the schema: Gitcko never writes it.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// The stage a run is in, as shown to the user while it works. The last five are
/// terminal: a run ends in exactly one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunLifecycle {
    Queued,
    Indexing,
    ReviewingFiles,
    GeneratingSummary,
    Completed,
    Failed,
    Canceled,
    Superseded,
    Skipped,
}

/// A circuit breaker for one model binding: `open` stops calls to a failing provider
/// so a run fails fast or falls back instead of waiting on timeouts; `half_open` lets
/// one trial call through to see whether it has recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

/// Which of the run's budgets (MODEL_ROUTING §6) a warning or stop is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BudgetLimit {
    Tokens,
    Cost,
    WallClock,
}

/// Where a result was posted on the code host: the summary, inline comments or the
/// check run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublishTarget {
    Summary,
    Inline,
    Check,
}
