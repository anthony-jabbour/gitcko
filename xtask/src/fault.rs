// SPDX-License-Identifier: AGPL-3.0-or-later

//! Suite F's injected faults, as a wrapper around the providers the router calls.
//! `cancel`, `deadline` and `token_budget` don't fake an error: they pull the run's own
//! token, deadline and output reservation, so the router and the engine react as they
//! would to the real thing. Faulted calls never reach the inner provider, so a
//! recording one writes nothing for them and a replaying one reads nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use futures_util::{StreamExt, stream};
use gitcko_model::{
    ErrorKind, Event, EventStream, FinishReason, ModelProvider, ProviderError, Request, Run,
    TimeoutKind,
};
use gitcko_protocol::config::ModelRole;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::eval::{Fault, FaultKind, FaultRole};

/// Shorter than the first-token timeout, so a call that hangs meets the run's deadline
/// first. Well under a minute, so a live run isn't left waiting on it either. The router
/// can't move a run's deadline once set, so it runs from the start: an earlier live call
/// slower than this ends the run before the fault, and the engine then fails the case.
const DEADLINE: Duration = Duration::from_secs(20);

/// One case's fault: the call count and the run token are shared by every binding.
#[derive(Clone)]
pub struct Injector {
    fault: Fault,
    calls: Arc<AtomicU32>,
    fired: Arc<AtomicBool>,
    cancel: CancellationToken,
}

impl Injector {
    pub fn new(fault: &Fault) -> Injector {
        Injector {
            fault: fault.clone(),
            calls: Arc::default(),
            fired: Arc::default(),
            cancel: CancellationToken::new(),
        }
    }

    /// Gives `run` what the fault pulls on: its cancel token, and for `deadline` a
    /// deadline the faulted call is sure to meet.
    pub fn arm(&self, run: &mut Run) {
        run.cancel = self.cancel.clone();
        if self.fault.kind == FaultKind::Deadline {
            run.deadline = Some(Instant::now() + DEADLINE);
        }
    }

    pub fn wrap(&self, inner: Arc<dyn ModelProvider>) -> Arc<dyn ModelProvider> {
        Arc::new(Faulty {
            inner,
            injector: self.clone(),
        })
    }

    /// Whether the fault was injected at all. A run that ended before the named call
    /// tested nothing, however its result looks.
    pub fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }

    /// Whether the `n`th call of the faulted role (1 is the first) is hit.
    fn hits(&self, n: u32) -> bool {
        let at = self.fault.at_call;
        match self.fault.kind {
            FaultKind::Malformed => n == at,
            // The repair call right after it fails too.
            FaultKind::InvalidRepair => n == at || n == at.saturating_add(1),
            _ => n >= at,
        }
    }
}

struct Faulty {
    inner: Arc<dyn ModelProvider>,
    injector: Injector,
}

fn fail(kind: ErrorKind) -> EventStream {
    stream::once(async move { Err(ProviderError::new(kind)) }).boxed()
}

impl ModelProvider for Faulty {
    fn stream(&self, request: Request, cancel: CancellationToken) -> EventStream {
        let it = &self.injector;
        let role = match it.fault.role {
            FaultRole::RiskClassifier => ModelRole::RiskClassifier,
            FaultRole::Review => ModelRole::Review,
            FaultRole::Judge => ModelRole::Judge,
        };
        if request.role != role || !it.hits(it.calls.fetch_add(1, Ordering::SeqCst) + 1) {
            return self.inner.stream(request, cancel);
        }
        it.fired.store(true, Ordering::SeqCst);
        match it.fault.kind {
            FaultKind::Timeout => fail(ErrorKind::Timeout(TimeoutKind::FirstToken)),
            FaultKind::Unavailable => fail(ErrorKind::Overloaded),
            FaultKind::Refusal => fail(ErrorKind::ContentPolicy),
            FaultKind::Malformed | FaultKind::InvalidRepair => stream::iter([
                Ok(Event::TextDelta {
                    text: "not json".into(),
                }),
                Ok(Event::Finish {
                    reason: FinishReason::Stop,
                }),
            ])
            .boxed(),
            // One byte past what the reservation allows (a token is three bytes), so the
            // router stops the call as it would a model that wouldn't stop.
            FaultKind::TokenBudget => {
                let bytes = (request.max_output as usize).saturating_mul(3) + 1;
                stream::iter([
                    Ok(Event::TextDelta {
                        text: "x".repeat(bytes),
                    }),
                    Ok(Event::Finish {
                        reason: FinishReason::Stop,
                    }),
                ])
                .boxed()
            }
            // Silent until the run's deadline arrives.
            FaultKind::Deadline => stream::pending().boxed(),
            FaultKind::Cancel => {
                it.cancel.cancel();
                stream::pending().boxed()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gitcko_model::{
        BudgetLimits, FakeProvider, Message, PromptRef, RunBudget, estimate_tokens,
    };

    const ALL: [FaultKind; 8] = [
        FaultKind::Timeout,
        FaultKind::Malformed,
        FaultKind::InvalidRepair,
        FaultKind::Unavailable,
        FaultKind::Refusal,
        FaultKind::TokenBudget,
        FaultKind::Deadline,
        FaultKind::Cancel,
    ];

    fn fault(kind: FaultKind, role: FaultRole, at_call: u32) -> Fault {
        Fault {
            kind,
            role,
            at_call,
        }
    }

    fn request(role: ModelRole, text: &str) -> Request {
        Request {
            role,
            prompt: PromptRef {
                id: "review".into(),
                version: "1".into(),
            },
            messages: vec![Message::User { text: text.into() }],
            tools: vec![],
            response_format: None,
            max_output: 16,
            reasoning_effort: None,
            temperature: None,
        }
    }

    fn good() -> Vec<Result<Event, ProviderError>> {
        vec![
            Ok(Event::TextDelta { text: "ok".into() }),
            Ok(Event::Finish {
                reason: FinishReason::Stop,
            }),
        ]
    }

    /// An inner provider that answers `n` calls well, and the wrapped view of it.
    fn rig(f: Fault, n: usize) -> (Arc<FakeProvider>, Injector, Arc<dyn ModelProvider>) {
        let inner = Arc::new(FakeProvider::new((0..n).map(|_| good()).collect()));
        let injector = Injector::new(&f);
        let wrapped = injector.wrap(inner.clone());
        (inner, injector, wrapped)
    }

    async fn call(p: &dyn ModelProvider, role: ModelRole) -> Vec<Result<Event, ProviderError>> {
        p.stream(request(role, "x"), CancellationToken::new())
            .collect()
            .await
    }

    fn passed(items: &[Result<Event, ProviderError>]) -> bool {
        *items == good()
    }

    #[tokio::test(start_paused = true)]
    async fn calls_before_at_call_pass_through_and_the_one_at_it_does_not() {
        for kind in ALL {
            let (inner, _, p) = rig(fault(kind, FaultRole::Review, 3), 5);
            for _ in 0..2 {
                assert!(passed(&call(&*p, ModelRole::Review).await), "{kind:?}");
            }
            assert_eq!(inner.calls(), 2, "{kind:?}");
            let third =
                tokio::time::timeout(Duration::from_secs(1), call(&*p, ModelRole::Review)).await;
            // Deadline and cancel say nothing at all, which is their fault.
            if let Ok(items) = third {
                assert!(!passed(&items), "{kind:?}");
            } else {
                assert!(matches!(kind, FaultKind::Deadline | FaultKind::Cancel));
            }
            assert_eq!(
                inner.calls(),
                2,
                "{kind:?}: a faulted call reached the inner"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn each_kind_fails_the_way_the_router_reads_it() {
        let error = |kind| {
            let (_, _, p) = rig(fault(kind, FaultRole::Review, 1), 1);
            call_now(p)
        };
        fn call_now(p: Arc<dyn ModelProvider>) -> Vec<Result<Event, ProviderError>> {
            futures_util::FutureExt::now_or_never(call(&*p, ModelRole::Review)).unwrap()
        }
        let kinds = |kind| -> Vec<ErrorKind> {
            error(kind)
                .into_iter()
                .filter_map(|r| r.err())
                .map(|e| e.kind)
                .collect()
        };
        assert_eq!(
            kinds(FaultKind::Timeout),
            [ErrorKind::Timeout(TimeoutKind::FirstToken)]
        );
        assert_eq!(kinds(FaultKind::Unavailable), [ErrorKind::Overloaded]);
        assert_eq!(kinds(FaultKind::Refusal), [ErrorKind::ContentPolicy]);
        for bad in [FaultKind::Malformed, FaultKind::InvalidRepair] {
            let items = error(bad);
            assert!(matches!(&items[0], Ok(Event::TextDelta { text }) if text == "not json"));
            assert!(matches!(items[1], Ok(Event::Finish { .. })));
        }
        // Positive control: an unfaulted call is the inner's reply.
        let (_, _, p) = rig(fault(FaultKind::Timeout, FaultRole::Review, 2), 1);
        assert!(passed(&call_now(p)));
    }

    #[tokio::test(start_paused = true)]
    async fn the_token_fault_outruns_the_reservation_by_one_token() {
        let (_, _, p) = rig(fault(FaultKind::TokenBudget, FaultRole::Review, 1), 1);
        let items = call(&*p, ModelRole::Review).await;
        let Ok(Event::TextDelta { text }) = &items[0] else {
            panic!("{items:?}")
        };
        assert_eq!(estimate_tokens(text.len()), 17);
    }

    #[tokio::test(start_paused = true)]
    async fn a_count_is_per_role_and_other_roles_are_never_faulted() {
        let (inner, _, p) = rig(fault(FaultKind::Unavailable, FaultRole::Review, 2), 6);
        for role in [ModelRole::RiskClassifier; 3] {
            assert!(passed(&call(&*p, role).await));
        }
        // The first review call is still the first, whatever came before it.
        assert!(passed(&call(&*p, ModelRole::Review).await));
        assert!(!passed(&call(&*p, ModelRole::Review).await));
        assert!(passed(&call(&*p, ModelRole::RiskClassifier).await));
        assert_eq!(inner.calls(), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn malformed_hits_one_call_invalid_repair_two_and_the_rest_last() {
        let run = |kind| async move {
            let (_, _, p) = rig(fault(kind, FaultRole::Review, 2), 6);
            let mut hit = Vec::new();
            for _ in 0..5 {
                hit.push(!passed(&call(&*p, ModelRole::Review).await));
            }
            hit
        };
        assert_eq!(
            run(FaultKind::Malformed).await,
            [false, true, false, false, false]
        );
        assert_eq!(
            run(FaultKind::InvalidRepair).await,
            [false, true, true, false, false]
        );
        assert_eq!(
            run(FaultKind::Unavailable).await,
            [false, true, true, true, true]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_cancels_the_runs_token_only_at_its_call() {
        let (_, injector, p) = rig(fault(FaultKind::Cancel, FaultRole::Review, 2), 2);
        let mut run = Run::new(
            "r",
            RunBudget::new(BudgetLimits {
                input_tokens: 1,
                output_tokens: 1,
                cost_usd: None,
            }),
        );
        injector.arm(&mut run);
        assert!(passed(&call(&*p, ModelRole::Review).await));
        assert!(!run.cancel.is_cancelled());
        let _pending = p.stream(request(ModelRole::Review, "x"), CancellationToken::new());
        assert!(run.cancel.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn only_the_deadline_fault_sets_a_deadline() {
        for kind in ALL {
            let injector = Injector::new(&fault(kind, FaultRole::Review, 1));
            let mut run = Run::new(
                "r",
                RunBudget::new(BudgetLimits {
                    input_tokens: 1,
                    output_tokens: 1,
                    cost_usd: None,
                }),
            );
            injector.arm(&mut run);
            assert_eq!(
                run.deadline.is_some(),
                kind == FaultKind::Deadline,
                "{kind:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn canary_faulted_replies_never_echo_the_request() {
        let secret = format!("{}-{}", "sk", "canary-value");
        for kind in ALL {
            let (_, _, p) = rig(fault(kind, FaultRole::Review, 1), 1);
            let stream = p.stream(
                request(ModelRole::Review, &secret),
                CancellationToken::new(),
            );
            let items: Vec<_> =
                tokio::time::timeout(Duration::from_secs(1), stream.collect::<Vec<_>>())
                    .await
                    .unwrap_or_default();
            assert!(!format!("{items:?}").contains(&secret), "{kind:?}");
        }
    }
}
