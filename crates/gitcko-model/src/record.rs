// SPDX-License-Identifier: AGPL-3.0-or-later

//! One record per call attempt (MODEL_ROUTING §12). It holds no prompt or response
//! text, no key and no endpoint beyond the provider's name, so it can go to the run
//! log and usage records as it is.

use gitcko_protocol::config::{ModelFamily, ModelProvider, ModelRole, ReasoningEffort};
use serde::Serialize;

use crate::InversionDecision;

/// The prompt template a call rendered (from `gitcko-prompts`, Phase 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptRef {
    pub id: String,
    pub version: String,
}

/// Why this attempt ran on a fallback, and which binding it replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fallback {
    pub from: String,
    /// A fixed reason code (`primary_failed`, `circuit_open`, ...), never free text.
    pub reason: &'static str,
}

/// A role's state in a run (MODEL_ROUTING §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleState {
    NotRequired,
    NotConfigured,
    NotRun,
    Running,
    Succeeded,
    FallbackUsed,
    Unavailable,
    InvalidOutput,
    Canceled,
    BudgetExhausted,
}

/// What one model call attempt did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallRecord {
    pub run_id: String,
    pub role: ModelRole,
    /// The routing policy as `id@version`.
    pub policy: String,
    pub provider: ModelProvider,
    pub model: String,
    pub family: ModelFamily,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub prompt: PromptRef,
    /// 1 for the first try.
    pub attempt: u32,
    pub fallback: Option<Fallback>,
    pub inversion: Option<InversionDecision>,
    /// Provider-reported; `None` is `unknown`, never zero.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    /// `None` when no token arrived.
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
    pub outcome: RoleState,
}
