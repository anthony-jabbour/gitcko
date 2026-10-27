// SPDX-License-Identifier: AGPL-3.0-or-later

//! Router I (MODEL_ROUTING §6–7) on one binding: what is retried and after how long,
//! timeouts at their exact virtual instant, cancellation, the circuit breaker and the
//! run budget. Every timing test runs on tokio's paused clock; nothing sleeps.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use gitcko_model::{
    BudgetLimits, CallError, CallOutcome, Destination, DestinationClass, ErrorKind, Event,
    EventStream, FakeProvider, FinishReason, Limit, Message, ModelProvider, PromptRef,
    ProviderError, Request, ResolvedBinding, ResolvedModels, RoleState, Router, Run, RunBudget,
    TimeoutKind, Usage, backoff,
};
use gitcko_protocol::config::{ModelFamily, ModelProvider as Provider, ModelRole};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const RUN: &str = "run-1";

fn binding(name: &str, class: DestinationClass) -> ResolvedBinding {
    let (provider, host) = match class {
        DestinationClass::OnDevice => (Provider::Ollama, "127.0.0.1"),
        DestinationClass::CustomerRemote => (Provider::Openrouter, "openrouter.ai"),
    };
    ResolvedBinding {
        name: name.into(),
        provider,
        model: format!("fixture/{name}"),
        family: ModelFamily::OpenWeight,
        reasoning_effort: None,
        data_policy: None,
        endpoint: None,
        context: None,
        destination: Destination {
            class,
            host: host.into(),
            port: 443,
        },
        credential: None,
    }
}

/// A router with one binding, `smart`, serving `review`.
fn router(class: DestinationClass, provider: Arc<dyn ModelProvider>) -> Router {
    let models = ResolvedModels {
        bindings: BTreeMap::from([("smart".to_string(), binding("smart", class))]),
        roles: BTreeMap::from([(ModelRole::Review, "smart".to_string())]),
        fallbacks: BTreeMap::new(),
        inversion: true,
    };
    Router::new(models, BTreeMap::from([("smart".to_string(), provider)]))
}

fn limits() -> BudgetLimits {
    BudgetLimits {
        input_tokens: 100_000,
        output_tokens: 10_000,
        cost_usd: None,
    }
}

fn run() -> Run {
    Run::new(RUN, RunBudget::new(limits()))
}

fn request() -> Request {
    Request {
        role: ModelRole::Review,
        prompt: PromptRef {
            id: "review".into(),
            version: "1".into(),
        },
        messages: vec![Message::User {
            text: "fn main() {}".into(),
        }],
        tools: vec![],
        response_format: None,
        max_output: 512,
        reasoning_effort: None,
        temperature: None,
    }
}

fn ok() -> Vec<Result<Event, ProviderError>> {
    vec![
        Ok(Event::TextDelta {
            text: "fine".into(),
        }),
        Ok(Event::Usage(Usage {
            input_tokens: Some(7),
            output_tokens: Some(2),
            cost_usd: None,
        })),
        Ok(Event::Finish {
            reason: FinishReason::Stop,
        }),
    ]
}

fn fail(kind: ErrorKind, status: Option<u16>) -> Vec<Result<Event, ProviderError>> {
    vec![Err(ProviderError { kind, status })]
}

fn last_error(outcome: &CallOutcome) -> Option<ErrorKind> {
    match &outcome.result {
        Err(CallError::Unavailable(e) | CallError::ProviderFailed(e)) => {
            e.as_ref().map(|e| e.kind.clone())
        }
        _ => None,
    }
}

/// One response: each item comes this many seconds after the previous one.
type Timed = Vec<(u64, Result<Event, ProviderError>)>;

/// A provider whose events arrive after scripted delays, which records each request
/// and counts dropped streams.
#[derive(Default)]
struct Slow {
    script: Mutex<VecDeque<Timed>>,
    requests: Mutex<Vec<Request>>,
    dropped: Arc<AtomicUsize>,
}

struct DropCount(Arc<AtomicUsize>);

impl Drop for DropCount {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Slow {
    /// Each event comes `secs` seconds after the previous one.
    fn new(script: Vec<Timed>) -> Arc<Slow> {
        Arc::new(Slow {
            script: Mutex::new(script.into()),
            ..Slow::default()
        })
    }
}

impl ModelProvider for Slow {
    fn stream(&self, request: Request, _cancel: CancellationToken) -> EventStream {
        self.requests.lock().unwrap().push(request);
        let events = self.script.lock().unwrap().pop_front().unwrap_or_default();
        let guard = DropCount(self.dropped.clone());
        futures_util::stream::unfold(
            (events.into_iter(), guard),
            |(mut events, guard)| async move {
                let (secs, item) = events.next()?;
                tokio::time::sleep(Duration::from_secs(secs)).await;
                Some((item, (events, guard)))
            },
        )
        .boxed()
    }
}

/// Never sends anything after the first delta.
const NEVER: u64 = 1_000_000;

#[tokio::test(start_paused = true)]
async fn what_may_pass_is_retried_and_what_cannot_is_not() {
    let retried = [
        (ErrorKind::RateLimited { retry_after: None }, Some(429)),
        (ErrorKind::Overloaded, Some(529)),
        (ErrorKind::Server, Some(500)),
        (ErrorKind::Server, Some(503)),
        (ErrorKind::Timeout(TimeoutKind::FirstToken), None),
        (ErrorKind::Connect, None),
        // OpenRouter's in-flight-budget 402, as adapters map it.
        (ErrorKind::RateLimited { retry_after: None }, Some(402)),
    ];
    for (kind, status) in retried {
        let fake = Arc::new(FakeProvider::new(vec![fail(kind.clone(), status), ok()]));
        let outcome = router(DestinationClass::CustomerRemote, fake.clone())
            .call(&mut run(), request())
            .await;
        assert!(outcome.result.is_ok(), "{kind:?}");
        assert_eq!(fake.calls(), 2, "{kind:?}");
    }
    let final_ = [
        (ErrorKind::Auth, Some(401)),
        (ErrorKind::Auth, Some(403)),
        (ErrorKind::BadRequest, Some(400)),
        (ErrorKind::ContentPolicy, Some(400)),
        (ErrorKind::ContextLength, Some(400)),
        (ErrorKind::InsufficientCredits, Some(402)),
        (ErrorKind::InvalidResponse("fixture"), None),
    ];
    for (kind, status) in final_ {
        let fake = Arc::new(FakeProvider::new(vec![fail(kind.clone(), status), ok()]));
        let outcome = router(DestinationClass::CustomerRemote, fake.clone())
            .call(&mut run(), request())
            .await;
        assert_eq!(last_error(&outcome), Some(kind.clone()));
        assert_eq!(fake.calls(), 1, "{kind:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn remote_retries_twice_and_local_once() {
    for (class, calls) in [
        (DestinationClass::CustomerRemote, 3),
        (DestinationClass::OnDevice, 2),
    ] {
        let script = (0..4).map(|_| fail(ErrorKind::Server, Some(500))).collect();
        let fake = Arc::new(FakeProvider::new(script));
        let outcome = router(class, fake.clone())
            .call(&mut run(), request())
            .await;
        assert_eq!(fake.calls(), calls);
        assert_eq!(outcome.records.len(), calls);
        let attempts: Vec<u32> = outcome.records.iter().map(|r| r.attempt).collect();
        assert_eq!(attempts, (1..=calls as u32).collect::<Vec<_>>());
        assert_eq!(outcome.state(), RoleState::Unavailable);
    }
}

#[tokio::test(start_paused = true)]
async fn retry_after_is_waited_exactly() {
    let after = ErrorKind::RateLimited {
        retry_after: Some(Duration::from_secs(7)),
    };
    let fake = Arc::new(FakeProvider::new(vec![fail(after, Some(429)), ok()]));
    let start = Instant::now();
    let outcome = router(DestinationClass::CustomerRemote, fake)
        .call(&mut run(), request())
        .await;
    assert!(outcome.result.is_ok());
    assert_eq!(start.elapsed(), Duration::from_secs(7));
}

#[tokio::test(start_paused = true)]
async fn a_retry_after_past_the_deadline_fails_now() {
    let after = ErrorKind::RateLimited {
        retry_after: Some(Duration::from_secs(7)),
    };
    let fake = Arc::new(FakeProvider::new(vec![
        fail(after.clone(), Some(429)),
        ok(),
    ]));
    let mut run = run();
    run.deadline = Some(Instant::now() + Duration::from_secs(5));
    let start = Instant::now();
    let outcome = router(DestinationClass::CustomerRemote, fake.clone())
        .call(&mut run, request())
        .await;
    assert_eq!(start.elapsed(), Duration::ZERO);
    assert_eq!(fake.calls(), 1);
    assert_eq!(last_error(&outcome), Some(after));
}

#[tokio::test(start_paused = true)]
async fn backoff_waits_exactly_the_jitter_values() {
    let fake = Arc::new(FakeProvider::new(vec![
        fail(ErrorKind::Server, Some(500)),
        fail(ErrorKind::Server, Some(500)),
        ok(),
    ]));
    let start = Instant::now();
    let outcome = router(DestinationClass::CustomerRemote, fake)
        .call(&mut run(), request())
        .await;
    assert!(outcome.result.is_ok());
    let expected = backoff(RUN, "smart", 1) + backoff(RUN, "smart", 2);
    assert_eq!(start.elapsed(), expected);
}

#[test]
fn backoff_is_full_jitter_under_a_doubling_cap_and_repeatable() {
    let mut seen = std::collections::BTreeSet::new();
    for retry in 1..=8u32 {
        let cap = Duration::from_secs(1 << (retry - 1).min(6));
        for run in 0..50 {
            let delay = backoff(&format!("run-{run}"), "smart", retry);
            assert!(delay <= cap, "retry {retry}: {delay:?} > {cap:?}");
            assert_eq!(delay, backoff(&format!("run-{run}"), "smart", retry));
            seen.insert(delay);
        }
    }
    // Spread out, not a constant.
    assert!(seen.len() > 300);
}

#[tokio::test(start_paused = true)]
async fn no_first_token_times_out_at_exactly_30_seconds_remote() {
    let late = || vec![(40, Ok(Event::TextDelta { text: "x".into() }))];
    let slow = Slow::new(vec![late(), late(), late()]);
    let outcome = router(DestinationClass::CustomerRemote, slow)
        .call(&mut run(), request())
        .await;
    assert_eq!(
        last_error(&outcome),
        Some(ErrorKind::Timeout(TimeoutKind::FirstToken))
    );
    for record in &outcome.records {
        assert_eq!(record.total_ms, 30_000);
        assert_eq!(record.first_token_ms, None);
    }
}

#[tokio::test(start_paused = true)]
async fn local_waits_120_seconds_for_a_first_token() {
    let late = || vec![(130, Ok(Event::TextDelta { text: "x".into() }))];
    let slow = Slow::new(vec![late(), late()]);
    let outcome = router(DestinationClass::OnDevice, slow)
        .call(&mut run(), request())
        .await;
    assert_eq!(outcome.records[0].total_ms, 120_000);
}

#[tokio::test(start_paused = true)]
async fn a_stream_times_out_in_total_and_is_not_retried_after_output() {
    let slow = Slow::new(vec![
        vec![
            (1, Ok(Event::TextDelta { text: "x".into() })),
            (NEVER, Ok(Event::TextDelta { text: "y".into() })),
        ],
        ok().into_iter().map(|e| (0, e)).collect(),
    ]);
    let outcome = router(DestinationClass::CustomerRemote, slow.clone())
        .call(&mut run(), request())
        .await;
    assert_eq!(
        last_error(&outcome),
        Some(ErrorKind::Timeout(TimeoutKind::Total))
    );
    assert_eq!(outcome.records.len(), 1, "no retry after partial output");
    assert_eq!(outcome.records[0].total_ms, 600_000);
    assert_eq!(outcome.records[0].first_token_ms, Some(1_000));
    assert_eq!(slow.dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_failure_after_output_is_not_retried() {
    let fake = Arc::new(FakeProvider::new(vec![
        vec![
            Ok(Event::TextDelta { text: "x".into() }),
            Err(ProviderError::new(ErrorKind::Connect)),
        ],
        ok(),
    ]));
    let outcome = router(DestinationClass::CustomerRemote, fake.clone())
        .call(&mut run(), request())
        .await;
    assert_eq!(fake.calls(), 1);
    assert_eq!(last_error(&outcome), Some(ErrorKind::Connect));
}

#[tokio::test(start_paused = true)]
async fn a_stream_without_finish_is_an_invalid_response() {
    let fake = Arc::new(FakeProvider::new(vec![vec![Ok(Event::TextDelta {
        text: "x".into(),
    })]]));
    let outcome = router(DestinationClass::CustomerRemote, fake)
        .call(&mut run(), request())
        .await;
    assert!(matches!(
        last_error(&outcome),
        Some(ErrorKind::InvalidResponse(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn cancel_mid_stream_stops_promptly_and_drops_the_stream() {
    let baseline = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let slow = Slow::new(vec![vec![
        (1, Ok(Event::TextDelta { text: "x".into() })),
        (NEVER, Ok(Event::TextDelta { text: "y".into() })),
    ]]);
    let mut run = run();
    let cancel = run.cancel.clone();
    let canceler = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        cancel.cancel();
    });
    let start = Instant::now();
    let outcome = router(DestinationClass::CustomerRemote, slow.clone())
        .call(&mut run, request())
        .await;
    assert_eq!(start.elapsed(), Duration::from_secs(5));
    assert_eq!(outcome.result.unwrap_err(), CallError::Canceled);
    assert_eq!(outcome.records.len(), 1, "a canceled call is not retried");
    assert_eq!(outcome.records[0].outcome, RoleState::Canceled);
    assert_eq!(slow.dropped.load(Ordering::SeqCst), 1);
    canceler.await.unwrap();
    let alive = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    assert_eq!(alive, baseline, "no task left running");
}

#[tokio::test(start_paused = true)]
async fn cancel_during_backoff_stops_without_another_attempt() {
    let fake = Arc::new(FakeProvider::new(vec![
        fail(
            ErrorKind::RateLimited {
                retry_after: Some(Duration::from_secs(30)),
            },
            Some(429),
        ),
        ok(),
    ]));
    let mut run = run();
    let cancel = run.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        cancel.cancel();
    });
    let start = Instant::now();
    let outcome = router(DestinationClass::CustomerRemote, fake.clone())
        .call(&mut run, request())
        .await;
    assert_eq!(
        start.elapsed(),
        Duration::from_secs(2),
        "not after the wait"
    );
    assert_eq!(outcome.state(), RoleState::Canceled);
    assert_eq!(fake.calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn the_deadline_ends_a_stream_at_its_instant() {
    let slow = Slow::new(vec![vec![
        (1, Ok(Event::TextDelta { text: "x".into() })),
        (NEVER, Ok(Event::TextDelta { text: "y".into() })),
    ]]);
    let mut run = run();
    run.deadline = Some(Instant::now() + Duration::from_secs(45));
    let start = Instant::now();
    let outcome = router(DestinationClass::CustomerRemote, slow)
        .call(&mut run, request())
        .await;
    assert_eq!(start.elapsed(), Duration::from_secs(45));
    assert_eq!(
        outcome.result.unwrap_err(),
        CallError::BudgetExhausted(Limit::Deadline)
    );
}

#[tokio::test(start_paused = true)]
async fn the_breaker_opens_on_the_third_failure_and_half_opens_after_60_seconds() {
    let fake = Arc::new(FakeProvider::new(vec![
        fail(ErrorKind::Server, Some(500)),
        fail(ErrorKind::Server, Some(500)),
        fail(ErrorKind::Server, Some(500)),
        // The half-open trial.
        ok(),
    ]));
    let router = router(DestinationClass::CustomerRemote, fake.clone());
    let first = router.call(&mut run(), request()).await;
    assert_eq!(fake.calls(), 3);
    assert_eq!(last_error(&first), Some(ErrorKind::Server));

    // Benched: refused without a call, and no error was made up for it.
    let benched = router.call(&mut run(), request()).await;
    assert_eq!(fake.calls(), 3);
    assert_eq!(benched.result.unwrap_err(), CallError::Unavailable(None));

    tokio::time::advance(Duration::from_secs(59)).await;
    let _ = router.call(&mut run(), request()).await;
    assert_eq!(fake.calls(), 3, "still benched at 59 s");

    tokio::time::advance(Duration::from_secs(1)).await;
    let trial = router.call(&mut run(), request()).await;
    assert_eq!(fake.calls(), 4, "one trial at 60 s");
    assert!(trial.result.is_ok());
}

#[tokio::test(start_paused = true)]
async fn a_failed_trial_benches_the_binding_again() {
    let script = (0..5).map(|_| fail(ErrorKind::Server, Some(500))).collect();
    let fake = Arc::new(FakeProvider::new(script));
    let router = router(DestinationClass::CustomerRemote, fake.clone());
    let _ = router.call(&mut run(), request()).await;
    tokio::time::advance(Duration::from_secs(60)).await;
    let _ = router.call(&mut run(), request()).await;
    assert_eq!(fake.calls(), 4, "the trial gets one attempt, no retries");
    let _ = router.call(&mut run(), request()).await;
    assert_eq!(fake.calls(), 4, "benched again");
}

#[tokio::test(start_paused = true)]
async fn request_faults_do_not_trip_the_breaker() {
    let script = (0..4)
        .map(|_| fail(ErrorKind::BadRequest, Some(400)))
        .chain([ok()])
        .collect();
    let fake = Arc::new(FakeProvider::new(script));
    let router = router(DestinationClass::CustomerRemote, fake.clone());
    for _ in 0..4 {
        let _ = router.call(&mut run(), request()).await;
    }
    assert!(router.call(&mut run(), request()).await.result.is_ok());
    assert_eq!(fake.calls(), 5);
}

#[tokio::test(start_paused = true)]
async fn output_past_the_reservation_is_stopped_mid_stream() {
    let slow = Slow::new(vec![vec![
        (
            0,
            Ok(Event::TextDelta {
                text: "a".repeat(30),
            }),
        ),
        (
            0,
            Ok(Event::TextDelta {
                text: "b".repeat(30),
            }),
        ),
        (NEVER, Ok(Event::TextDelta { text: "c".into() })),
    ]]);
    let mut run = Run::new(
        RUN,
        RunBudget::new(BudgetLimits {
            output_tokens: 15,
            ..limits()
        }),
    );
    let outcome = router(DestinationClass::CustomerRemote, slow.clone())
        .call(&mut run, request())
        .await;
    assert_eq!(
        outcome.result.unwrap_err(),
        CallError::BudgetExhausted(Limit::OutputTokens)
    );
    // The provider was told the trimmed limit too.
    assert_eq!(slow.requests.lock().unwrap()[0].max_output, 15);
    assert_eq!(slow.dropped.load(Ordering::SeqCst), 1);
    // 60 bytes streamed, charged at the estimate.
    assert_eq!(run.budget.spent().output_tokens, 20);
}

#[tokio::test(start_paused = true)]
async fn a_prompt_that_does_not_fit_is_refused_before_sending() {
    let fake = Arc::new(FakeProvider::new(vec![ok()]));
    let mut run = Run::new(
        RUN,
        RunBudget::new(BudgetLimits {
            input_tokens: 5,
            ..limits()
        }),
    );
    let outcome = router(DestinationClass::CustomerRemote, fake.clone())
        .call(&mut run, request())
        .await;
    assert_eq!(outcome.state(), RoleState::BudgetExhausted);
    assert_eq!(
        outcome.result.unwrap_err(),
        CallError::BudgetExhausted(Limit::InputTokens)
    );
    assert_eq!(fake.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn retries_spend_from_the_same_budget() {
    let prompt = gitcko_model::estimate_tokens(serde_json::to_vec(&request()).unwrap().len());
    let fake = Arc::new(FakeProvider::new(vec![
        fail(ErrorKind::Server, Some(500)),
        ok(),
    ]));
    // Room for one prompt and a half: the failed attempt is charged, the retry can't fit.
    let mut run = Run::new(
        RUN,
        RunBudget::new(BudgetLimits {
            input_tokens: prompt * 3 / 2,
            ..limits()
        }),
    );
    let outcome = router(DestinationClass::CustomerRemote, fake.clone())
        .call(&mut run, request())
        .await;
    assert_eq!(fake.calls(), 1);
    assert_eq!(
        outcome.result.unwrap_err(),
        CallError::BudgetExhausted(Limit::InputTokens)
    );
    assert_eq!(run.budget.spent().input_tokens, prompt);
}

#[tokio::test(start_paused = true)]
async fn a_success_settles_reported_usage_and_records_the_attempt() {
    let fake = Arc::new(FakeProvider::new(vec![ok()]));
    let mut run = run();
    let outcome = router(DestinationClass::CustomerRemote, fake)
        .call(&mut run, request())
        .await;
    let response = outcome.result.as_ref().unwrap();
    assert_eq!(response.text, "fine");
    assert_eq!(response.binding, "smart");
    assert_eq!(outcome.state(), RoleState::Succeeded);
    assert_eq!(run.budget.spent().input_tokens, 7);
    assert_eq!(run.budget.spent().output_tokens, 2);
    let record = &outcome.records[0];
    assert_eq!(
        (record.run_id.as_str(), record.attempt, record.input_tokens),
        (RUN, 1, Some(7))
    );
    assert_eq!(record.policy, "gitcko-std@1");
    assert_eq!(record.outcome, RoleState::Succeeded);
}

#[tokio::test(start_paused = true)]
async fn a_role_without_a_binding_is_not_configured() {
    let fake = Arc::new(FakeProvider::new(vec![ok()]));
    let mut judge = request();
    judge.role = ModelRole::Judge;
    let outcome = router(DestinationClass::CustomerRemote, fake.clone())
        .call(&mut run(), judge)
        .await;
    assert_eq!(outcome.state(), RoleState::NotConfigured);
    assert_eq!(fake.calls(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_required_role_fails_the_run_and_an_optional_one_is_unavailable() {
    for required in [true, false] {
        let fake = Arc::new(FakeProvider::new(vec![fail(ErrorKind::Auth, Some(401))]));
        let mut run = run();
        if required {
            run.required.push(ModelRole::Review);
        }
        let outcome = router(DestinationClass::CustomerRemote, fake)
            .call(&mut run, request())
            .await;
        let error = Some(ProviderError {
            kind: ErrorKind::Auth,
            status: Some(401),
        });
        let expected = if required {
            CallError::ProviderFailed(error)
        } else {
            CallError::Unavailable(error)
        };
        assert_eq!(outcome.result.unwrap_err(), expected);
    }
}

#[tokio::test(start_paused = true)]
async fn a_trial_whose_caller_gives_up_frees_the_trial_for_the_next_call() {
    let error = || vec![(0, Err(ProviderError::new(ErrorKind::Server)))];
    let slow = Slow::new(vec![
        error(),
        error(),
        error(),
        vec![(NEVER, Ok(Event::TextDelta { text: "x".into() }))],
        ok().into_iter().map(|e| (0, e)).collect(),
    ]);
    let router = router(DestinationClass::CustomerRemote, slow.clone());
    let _ = router.call(&mut run(), request()).await;
    tokio::time::advance(Duration::from_secs(60)).await;
    // The trial's caller drops it after a second.
    let mut abandoned = run();
    let gave_up = tokio::time::timeout(
        Duration::from_secs(1),
        router.call(&mut abandoned, request()),
    )
    .await;
    assert!(gave_up.is_err());
    let next = router.call(&mut run(), request()).await;
    assert!(next.result.is_ok(), "the next call gets the trial");
    assert_eq!(slow.requests.lock().unwrap().len(), 5);
}

/// A provider's `Retry-After` is untrusted: one longer than the attempt's own total
/// timeout isn't waited for, with or without a deadline, and never overflows.
#[tokio::test(start_paused = true)]
async fn an_absurd_retry_after_fails_now_instead_of_waiting() {
    for after in [Duration::from_secs(601), Duration::MAX] {
        let kind = ErrorKind::RateLimited {
            retry_after: Some(after),
        };
        let fake = Arc::new(FakeProvider::new(vec![fail(kind.clone(), Some(429)), ok()]));
        let start = Instant::now();
        let outcome = router(DestinationClass::CustomerRemote, fake.clone())
            .call(&mut run(), request())
            .await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert_eq!(fake.calls(), 1);
        assert_eq!(last_error(&outcome), Some(kind));
    }
}

/// Dropping a call mid-attempt (the caller timed it out) still settles the attempt,
/// so its reservation doesn't hold the run's budget forever.
#[tokio::test(start_paused = true)]
async fn an_abandoned_call_releases_its_reservation() {
    let slow = Slow::new(vec![
        vec![(NEVER, Ok(Event::TextDelta { text: "x".into() }))],
        ok().into_iter().map(|e| (0, e)).collect(),
    ]);
    let router = router(DestinationClass::CustomerRemote, slow);
    // Room for exactly one call's output.
    let mut run = Run::new(
        RUN,
        RunBudget::new(BudgetLimits {
            output_tokens: 512,
            ..limits()
        }),
    );
    let abandoned =
        tokio::time::timeout(Duration::from_secs(1), router.call(&mut run, request())).await;
    assert!(abandoned.is_err());
    let next = router.call(&mut run, request()).await;
    assert!(next.result.is_ok(), "{:?}", next.result);
    // The abandoned attempt was charged its prompt estimate, not refunded.
    let prompt = gitcko_model::estimate_tokens(serde_json::to_vec(&request()).unwrap().len());
    assert_eq!(run.budget.spent().input_tokens, prompt + 7);
}

/// Usage or an empty delta isn't output: the first-token limit still applies, and a
/// later failure may still be retried.
#[tokio::test(start_paused = true)]
async fn metadata_is_not_a_first_token() {
    let slow = Slow::new(vec![
        vec![
            (1, Ok(Event::Usage(Usage::default()))),
            (
                0,
                Ok(Event::TextDelta {
                    text: String::new(),
                }),
            ),
            (NEVER, Ok(Event::TextDelta { text: "x".into() })),
        ],
        ok().into_iter().map(|e| (0, e)).collect(),
    ]);
    let outcome = router(DestinationClass::CustomerRemote, slow)
        .call(&mut run(), request())
        .await;
    assert!(
        outcome.result.is_ok(),
        "retried after the first-token timeout"
    );
    assert_eq!(outcome.records[0].total_ms, 30_000);
    assert_eq!(outcome.records[0].first_token_ms, None);
}
