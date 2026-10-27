// SPDX-License-Identifier: AGPL-3.0-or-later

//! The model layer's core (MODEL_ROUTING). Code asks for a **role**, never a model: the
//! router ([`Router`]) turns the role into a binding resolved here, checks
//! where the call would go, spends from the run's budget and records what happened.
//!
//! Nothing here does I/O. The provider adapters (`gitcko-model-openai`,
//! `gitcko-model-ollama`) implement [`ModelProvider`]; `gitcko-cassette` records and
//! replays at the same boundary, so tests never call a live model.

mod budget;
#[cfg(feature = "conformance")]
pub mod conformance;
mod error;
mod family;
mod provider;
mod record;
mod request;
mod resolve;
mod router;
mod secret;
pub mod stream;

pub use budget::{BudgetLimits, Reservation, RunBudget, Spent, estimate_tokens};
pub use error::{ErrorKind, Limit, ProviderError, ResolveError, TimeoutKind};
pub use family::{
    AgentFamilies, AuthorFamily, AuthorSignals, InversionDecision, Signal, author_family,
};
#[cfg(feature = "fake")]
pub use provider::FakeProvider;
pub use provider::{EventStream, ModelProvider};
pub use record::{CallRecord, Fallback, PromptRef, RoleState};
pub use request::{Event, FinishReason, Message, Request, ResponseFormat, Tool, ToolCall, Usage};
pub use resolve::{
    Capabilities, Destination, DestinationClass, Disclosed, Disclosure, ResolvedBinding,
    ResolvedModels, class_of, disclose, is_loopback_host, resolve,
};
pub use router::{
    CallError, CallOutcome, POLICY, Response, Router, Run, Timeouts, backoff, retries, retryable,
    timeouts,
};
pub use secret::Secret;

/// The serde spelling of a config enum (`review-deep`, `smart`), for messages and
/// records, so they match what the user wrote in `[models]`.
pub(crate) fn wire_id(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}
