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
#[allow(
    clippy::large_enum_variant,
    reason = "a run emits tens of events, each serialized once; boxing a variant would only complicate the wire type"
)]
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
    /// One model call attempt, from the router's call record (MODEL_ROUTING §12). Values
    /// are names, codes and numbers only: never prompt or response text, keys or hosts.
    #[serde(rename = "model_call.completed")]
    ModelCallCompleted {
        step: Option<String>,
        role: String,
        binding: String,
        class: String,
        provider: String,
        /// The model family, which judge independence and inversion compare.
        family: String,
        reasoning_effort: Option<String>,
        /// The routing policy as `id@version`.
        policy: String,
        prompt: PromptRef,
        /// 1 for the first try.
        attempt: u32,
        /// The binding this call fell back from, if it was a fallback.
        fallback_from: Option<String>,
        /// A fixed code such as `primary_failed` or `circuit_open`, set with `fallback_from`.
        fallback_reason: Option<String>,
        /// The author-family inversion decision, for roles that apply it.
        inversion: Option<String>,
        /// `null` when the provider didn't report it; never 0 for unknown.
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        /// `null` when the provider doesn't report cost.
        cost_usd: Option<f64>,
        /// `null` when no token arrived.
        first_token_ms: Option<u64>,
        duration_ms: u64,
        outcome: String,
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

/// A versioned prompt: changing its text changes its version, and with it every
/// recorded replay keyed on it (DEVELOPMENT §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PromptRef {
    pub id: String,
    pub version: String,
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
