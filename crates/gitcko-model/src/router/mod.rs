// SPDX-License-Identifier: AGPL-3.0-or-later

//! Every model call goes through the router (MODEL_ROUTING §1). The caller names a
//! role in the [`Request`]; the router picks the binding, reserves budget, enforces
//! timeouts, the deadline and cancellation, retries what may pass, benches failing
//! bindings, and returns one [`CallRecord`] per attempt.
//!
//! The router collects a call's events into a [`Response`]. ponytail: no streaming to
//! the caller yet; P4 adds it if the engine needs partial output.

mod breaker;
mod fallback;
mod inversion;
mod retry;

use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::StreamExt;
use gitcko_protocol::config::{ModelFamily, ModelRole};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use retry::{Timeouts, backoff, retries, retryable, timeouts};

use crate::{
    AuthorFamily, CallRecord, ErrorKind, Event, Fallback, FinishReason, Limit, ModelProvider,
    ProviderError, Request, ResolvedBinding, ResolvedModels, RoleState, RunBudget, TimeoutKind,
    ToolCall, Usage,
};

/// The routing policy calls are recorded under (MODEL_ROUTING §5).
pub const POLICY: &str = "gitcko-std@1";

/// One run's state, shared by every call it makes.
#[derive(Debug)]
pub struct Run {
    pub id: String,
    pub budget: RunBudget,
    /// The wall-clock deadline; `None` when the run has none.
    pub deadline: Option<Instant>,
    /// Canceling stops every call of the run.
    pub cancel: CancellationToken,
    /// Roles whose failure fails the run (MODEL_ROUTING §2, "Required?"). Which ones
    /// depends on the tier and Verify, so the caller says.
    pub required: Vec<ModelRole>,
    /// Only on-device bindings may receive a call, fallbacks included.
    pub local_only: bool,
    /// Who wrote the PR, for model inversion ([`crate::author_family`]).
    pub author: AuthorFamily,
    /// The family that last answered `review` or `review-deep`; `judge` prefers
    /// another. Set by the router.
    pub reviewer: Option<ModelFamily>,
}

impl Run {
    /// A run with no deadline, no required roles, remote calls allowed and an
    /// unknown author.
    pub fn new(id: impl Into<String>, budget: RunBudget) -> Run {
        Run {
            id: id.into(),
            budget,
            deadline: None,
            cancel: CancellationToken::new(),
            required: Vec::new(),
            local_only: false,
            author: AuthorFamily::Unknown,
            reviewer: None,
        }
    }
}

/// A finished call's output.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// The last usage the provider reported; `None` fields are `unknown`.
    pub usage: Usage,
    pub finish: FinishReason,
    /// The binding that answered.
    pub binding: String,
}

/// Why a call produced no response.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CallError {
    #[error("no model binding is configured for this role")]
    NotConfigured,
    /// A required role ran out of bindings to try; the run fails as `provider_failed`.
    /// The error is the last attempt's, `None` when no binding could be tried.
    #[error("the required model call failed{}", last_error(.0))]
    ProviderFailed(Option<ProviderError>),
    /// An optional role ran out of bindings; the caller takes its fallback path.
    #[error("the model call is unavailable{}", last_error(.0))]
    Unavailable(Option<ProviderError>),
    #[error("canceled")]
    Canceled,
    #[error("budget exhausted: {0}")]
    BudgetExhausted(Limit),
}

fn last_error(error: &Option<ProviderError>) -> String {
    error.as_ref().map(|e| format!(": {e}")).unwrap_or_default()
}

/// A call's result and what each attempt did.
#[derive(Debug)]
pub struct CallOutcome {
    pub result: Result<Response, CallError>,
    pub records: Vec<CallRecord>,
}

impl CallOutcome {
    /// The role's state for this call (MODEL_ROUTING §8). `fallback_used` when the
    /// answer came from a binding other than the first one tried.
    pub fn state(&self) -> RoleState {
        match &self.result {
            Ok(_) if self.records.last().is_some_and(|r| r.fallback.is_some()) => {
                RoleState::FallbackUsed
            }
            Ok(_) => RoleState::Succeeded,
            Err(CallError::NotConfigured) => RoleState::NotConfigured,
            Err(CallError::ProviderFailed(_) | CallError::Unavailable(_)) => RoleState::Unavailable,
            Err(CallError::Canceled) => RoleState::Canceled,
            Err(CallError::BudgetExhausted(_)) => RoleState::BudgetExhausted,
        }
    }
}

/// Routes calls for one set of resolved bindings. Shared by every run in the
/// process, so the circuit breakers see every run's failures.
pub struct Router {
    models: ResolvedModels,
    /// Binding name → the provider that runs it.
    providers: BTreeMap<String, Arc<dyn ModelProvider>>,
    breakers: breaker::Breakers,
}

/// What ended an attempt that didn't answer.
enum Stop {
    /// The run is over for this call: canceled, out of budget or past its deadline.
    Run(CallError),
    /// The binding failed. `partial` when output had already arrived, which rules
    /// out a retry: the same prompt again could answer differently.
    Failed { error: ProviderError, partial: bool },
    /// The circuit breaker refused the binding; the error is the last attempt's, if any.
    Benched(Option<ProviderError>),
}

impl Router {
    /// `providers` maps each binding name to its adapter (or a cassette in tests); a
    /// binding without one is treated as not configured.
    pub fn new(
        models: ResolvedModels,
        providers: BTreeMap<String, Arc<dyn ModelProvider>>,
    ) -> Router {
        Router {
            models,
            providers,
            breakers: breaker::Breakers::default(),
        }
    }

    /// Runs `request` for its role: the planned bindings in order, each with its
    /// retries, until one answers or the run stops. Never panics on a provider's
    /// behaviour; every failure is a [`CallError`].
    pub async fn call(&self, run: &mut Run, request: Request) -> CallOutcome {
        let mut records = Vec::new();
        let role = request.role;
        let Some(plan) = fallback::plan(&self.models, run, role) else {
            return outcome(Err(CallError::NotConfigured), records);
        };
        // Why the next binding tried stands in for another, and which one.
        let mut fallback = plan.skipped_primary.map(|from| Fallback {
            from: from.to_owned(),
            reason: "locality",
        });
        let mut last = plan
            .candidates
            .is_empty()
            .then(|| ProviderError::new(ErrorKind::LocalityViolation));
        let mut tried_any = false;
        for binding in plan.candidates.iter().take(plan.tries) {
            let Some(provider) = self.providers.get(&binding.name) else {
                continue;
            };
            tried_any = true;
            let first = records.len();
            let result = self
                .try_binding(run, binding, provider.as_ref(), &request, &mut records)
                .await;
            for record in &mut records[first..] {
                record.fallback.clone_from(&fallback);
                record.inversion = plan.inversion;
            }
            let reason = match result {
                Ok(response) => {
                    if matches!(role, ModelRole::Review | ModelRole::ReviewDeep) {
                        run.reviewer = Some(binding.family);
                    }
                    return outcome(Ok(response), records);
                }
                Err(Stop::Run(error)) => return outcome(Err(error), records),
                // Output then a failure isn't retried on the same binding, but the
                // caller only gets whole responses, so another binding may answer.
                Err(Stop::Failed { error, partial }) => {
                    last = Some(error);
                    if partial { "partial_output" } else { "failed" }
                }
                Err(Stop::Benched(previous)) => {
                    last = previous.or(last);
                    "circuit_open"
                }
            };
            fallback = Some(Fallback {
                from: binding.name.clone(),
                reason,
            });
        }
        if !tried_any && last.is_none() {
            return outcome(Err(CallError::NotConfigured), records);
        }
        outcome(Err(give_up(run, role, last)), records)
    }

    /// The first attempt and its retries on one binding.
    async fn try_binding(
        &self,
        run: &mut Run,
        binding: &ResolvedBinding,
        provider: &dyn ModelProvider,
        request: &Request,
        records: &mut Vec<CallRecord>,
    ) -> Result<Response, Stop> {
        let mut last = None;
        for attempt in 1..=retries(binding.destination.class) + 1 {
            if attempt > 1 {
                // Wait for `Retry-After` when the provider sent one, else back off.
                let wait = match &last {
                    Some(ProviderError {
                        kind:
                            ErrorKind::RateLimited {
                                retry_after: Some(after),
                            },
                        ..
                    }) => *after,
                    _ => backoff(&run.id, &binding.name, attempt - 1),
                };
                // `Retry-After` is the provider's word: longer than an attempt may run
                // (or past the deadline) isn't worth sleeping through only to fail.
                let limit = timeouts(binding.destination.class, request.role).total;
                let wake = Instant::now().checked_add(wait).filter(|_| wait <= limit);
                let Some(wake) = wake.filter(|wake| run.deadline.is_none_or(|d| *wake <= d)) else {
                    let error = last.expect("a retry follows a failure");
                    return Err(Stop::Failed {
                        error,
                        partial: false,
                    });
                };
                tokio::select! {
                    () = run.cancel.cancelled() => return Err(Stop::Run(CallError::Canceled)),
                    () = tokio::time::sleep_until(wake) => {}
                }
            }
            let Some(permit) = self.breakers.admit(&binding.name) else {
                return Err(Stop::Benched(last));
            };
            let started = Instant::now();
            let (result, first_token, usage) = attempt_once(run, binding, provider, request).await;
            let failure = match &result {
                Ok(_) => None,
                Err(Stop::Failed { error, .. }) => Some(error.kind.clone()),
                Err(Stop::Run(CallError::Canceled)) => Some(ErrorKind::Canceled),
                Err(Stop::Run(_) | Stop::Benched(_)) => {
                    Some(ErrorKind::BudgetExhausted(Limit::Deadline))
                }
            };
            permit.finish(failure.as_ref());
            records.push(record(
                run,
                binding,
                request,
                attempt,
                &result,
                usage,
                first_token.map(|t| t - started),
                started.elapsed(),
            ));
            match result {
                Ok(response) => return Ok(response),
                Err(Stop::Failed { error, partial }) if retryable(&error.kind) && !partial => {
                    last = Some(error);
                }
                Err(stop) => return Err(stop),
            }
        }
        let error = last.expect("the loop returns unless an attempt failed");
        Err(Stop::Failed {
            error,
            partial: false,
        })
    }
}

/// One attempt: reserve, stream with timeouts, stop at the output reservation, settle.
/// Returns the result, when the first event arrived, and the reported usage.
async fn attempt_once(
    run: &mut Run,
    binding: &ResolvedBinding,
    provider: &dyn ModelProvider,
    request: &Request,
) -> (Result<Response, Stop>, Option<Instant>, Usage) {
    if run.cancel.is_cancelled() {
        return (Err(Stop::Run(CallError::Canceled)), None, Usage::default());
    }
    let started = Instant::now();
    if run.deadline.is_some_and(|deadline| started >= deadline) {
        let error = CallError::BudgetExhausted(Limit::Deadline);
        return (Err(Stop::Run(error)), None, Usage::default());
    }
    let prompt_bytes = serde_json::to_vec(request).map_or(0, |b| b.len());
    let reservation = match run
        .budget
        .reserve(prompt_bytes, u64::from(request.max_output))
    {
        Ok(reservation) => reservation,
        Err(limit) => {
            let error = CallError::BudgetExhausted(limit);
            return (Err(Stop::Run(error)), None, Usage::default());
        }
    };
    let max_output = reservation.max_output();
    let mut held = Held {
        budget: &mut run.budget,
        reservation: Some(reservation),
        usage: Usage::default(),
        streamed: 0,
    };
    let mut request = request.clone();
    request.max_output = u32::try_from(max_output).unwrap_or(u32::MAX);

    let limits = timeouts(binding.destination.class, request.role);
    let total_at = started + limits.total;
    let call_cancel = run.cancel.child_token();
    let mut stream = provider.stream(request, call_cancel.clone());
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    // When output (text or a tool call) first arrived. Usage and empty deltas aren't
    // output, so they don't stop the first-token clock or rule out a retry.
    let mut first_token = None;

    let result = loop {
        let (limit_at, timeout) = match first_token {
            None => (
                (started + limits.first_token).min(total_at),
                TimeoutKind::FirstToken,
            ),
            Some(_) => (total_at, TimeoutKind::Total),
        };
        // The run's deadline wins when it is sooner.
        let deadline_first = run.deadline.is_some_and(|deadline| deadline <= limit_at);
        let wake = run
            .deadline
            .map_or(limit_at, |deadline| deadline.min(limit_at));
        let partial = first_token.is_some();
        let item = tokio::select! {
            () = run.cancel.cancelled() => break Err(Stop::Run(CallError::Canceled)),
            item = tokio::time::timeout_at(wake, stream.next()) => item,
        };
        let item = match item {
            Err(_elapsed) if deadline_first => {
                break Err(Stop::Run(CallError::BudgetExhausted(Limit::Deadline)));
            }
            Err(_elapsed) => {
                let error = ProviderError::new(ErrorKind::Timeout(timeout));
                break Err(Stop::Failed { error, partial });
            }
            Ok(None) => {
                let error = ProviderError::new(ErrorKind::InvalidResponse(
                    "the stream ended without a finish event",
                ));
                break Err(Stop::Failed { error, partial });
            }
            Ok(Some(Err(error))) => break Err(Stop::Failed { error, partial }),
            Ok(Some(Ok(event))) => event,
        };
        match item {
            Event::TextDelta { text: delta } => {
                if !delta.is_empty() {
                    first_token.get_or_insert_with(Instant::now);
                }
                held.streamed += delta.len();
                text.push_str(&delta);
            }
            Event::ToolCall(call) => {
                first_token.get_or_insert_with(Instant::now);
                held.streamed += call.name.len() + call.arguments.len();
                tool_calls.push(call);
            }
            Event::Usage(reported) => held.usage = reported,
            Event::Finish { reason } => {
                break Ok(Response {
                    text: std::mem::take(&mut text),
                    tool_calls: std::mem::take(&mut tool_calls),
                    usage: held.usage,
                    finish: reason,
                    binding: binding.name.clone(),
                });
            }
        }
        if crate::estimate_tokens(held.streamed) > max_output {
            // Past what the budget reserved: stop paying for output now.
            break Err(Stop::Run(CallError::BudgetExhausted(Limit::OutputTokens)));
        }
    };
    // Stop the provider's work and drop its connection before settling.
    call_cancel.cancel();
    drop(stream);
    let usage = held.usage;
    drop(held);
    (result, first_token, usage)
}

/// An attempt's reservation, settled when dropped: normally at the end of the
/// attempt, but also when the caller drops the call mid-attempt, so an abandoned call
/// still charges what it used and releases what it held.
struct Held<'a> {
    budget: &'a mut RunBudget,
    reservation: Option<crate::Reservation>,
    usage: Usage,
    streamed: usize,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            self.budget.settle(reservation, &self.usage, self.streamed);
        }
    }
}

/// Out of bindings: a required role fails the run, an optional one is unavailable.
fn give_up(run: &Run, role: ModelRole, last: Option<ProviderError>) -> CallError {
    if run.required.contains(&role) {
        CallError::ProviderFailed(last)
    } else {
        CallError::Unavailable(last)
    }
}

fn outcome(result: Result<Response, CallError>, records: Vec<CallRecord>) -> CallOutcome {
    CallOutcome { result, records }
}

#[allow(clippy::too_many_arguments)]
fn record(
    run: &Run,
    binding: &ResolvedBinding,
    request: &Request,
    attempt: u32,
    result: &Result<Response, Stop>,
    usage: Usage,
    first_token: Option<std::time::Duration>,
    total: std::time::Duration,
) -> CallRecord {
    let millis = |d: std::time::Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    CallRecord {
        run_id: run.id.clone(),
        role: request.role,
        policy: POLICY.to_owned(),
        provider: binding.provider,
        model: binding.model.clone(),
        family: binding.family,
        reasoning_effort: binding.reasoning_effort,
        prompt: request.prompt.clone(),
        attempt,
        fallback: None,
        inversion: None,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cost_usd: usage.cost_usd,
        first_token_ms: first_token.map(millis),
        total_ms: millis(total),
        outcome: match result {
            Ok(_) => RoleState::Succeeded,
            Err(Stop::Run(CallError::Canceled)) => RoleState::Canceled,
            Err(Stop::Run(CallError::BudgetExhausted(_))) => RoleState::BudgetExhausted,
            Err(_) => RoleState::Unavailable,
        },
    }
}
