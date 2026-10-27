// SPDX-License-Identifier: AGPL-3.0-or-later

//! Router II (MODEL_ROUTING §8–9, §11): which bindings a call may fall back to and
//! why, locality as a hard limit, model inversion and judge independence, and the
//! role state every outcome maps to.

use std::collections::BTreeMap;
use std::sync::Arc;

use gitcko_model::{
    AuthorFamily, BudgetLimits, CallError, CallOutcome, Destination, DestinationClass, ErrorKind,
    Event, FakeProvider, Fallback, FinishReason, InversionDecision, Limit, Message, ModelProvider,
    PromptRef, ProviderError, Request, ResolvedBinding, ResolvedModels, RoleState, Router, Run,
    RunBudget, Signal,
};
use gitcko_protocol::config::{ModelClass, ModelFamily, ModelProvider as Provider, ModelRole};
use proptest::prelude::*;

use DestinationClass::{CustomerRemote as Remote, OnDevice as Local};

fn binding(name: &str, class: DestinationClass, family: ModelFamily) -> ResolvedBinding {
    let (provider, host) = match class {
        Local => (Provider::Ollama, "127.0.0.1"),
        Remote => (Provider::Openrouter, "openrouter.ai"),
    };
    ResolvedBinding {
        name: name.into(),
        provider,
        model: format!("fixture/{name}"),
        family,
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

/// Bindings with their scripted providers, and the routes over them.
struct Setup {
    bindings: Vec<ResolvedBinding>,
    fakes: BTreeMap<String, Arc<FakeProvider>>,
    roles: BTreeMap<ModelRole, String>,
    fallbacks: BTreeMap<ModelClass, Vec<String>>,
    inversion: bool,
}

impl Setup {
    fn new() -> Setup {
        Setup {
            bindings: vec![],
            fakes: BTreeMap::new(),
            roles: BTreeMap::new(),
            fallbacks: BTreeMap::new(),
            inversion: true,
        }
    }

    fn binding(
        mut self,
        name: &str,
        class: DestinationClass,
        family: ModelFamily,
        script: Vec<Vec<Result<Event, ProviderError>>>,
    ) -> Setup {
        self.bindings.push(binding(name, class, family));
        self.fakes
            .insert(name.into(), Arc::new(FakeProvider::new(script)));
        self
    }

    fn role(mut self, role: ModelRole, binding: &str) -> Setup {
        self.roles.insert(role, binding.into());
        self
    }

    fn fallbacks(mut self, class: ModelClass, names: &[&str]) -> Setup {
        self.fallbacks
            .insert(class, names.iter().map(|n| n.to_string()).collect());
        self
    }

    fn router(&self) -> Router {
        let models = ResolvedModels {
            bindings: self
                .bindings
                .iter()
                .map(|b| (b.name.clone(), b.clone()))
                .collect(),
            roles: self.roles.clone(),
            fallbacks: self.fallbacks.clone(),
            inversion: self.inversion,
        };
        let providers = self
            .fakes
            .iter()
            .map(|(name, fake)| (name.clone(), fake.clone() as Arc<dyn ModelProvider>))
            .collect();
        Router::new(models, providers)
    }

    fn calls(&self, name: &str) -> usize {
        self.fakes[name].calls()
    }
}

fn limits() -> BudgetLimits {
    BudgetLimits {
        input_tokens: 100_000,
        output_tokens: 10_000,
        cost_usd: None,
    }
}

fn run() -> Run {
    Run::new("run-1", RunBudget::new(limits()))
}

fn request(role: ModelRole) -> Request {
    Request {
        role,
        prompt: PromptRef {
            id: "review".into(),
            version: "1".into(),
        },
        messages: vec![Message::User {
            text: "fn main() {}".into(),
        }],
        tools: vec![],
        response_format: None,
        max_output: 256,
        reasoning_effort: None,
        temperature: None,
    }
}

fn ok() -> Vec<Result<Event, ProviderError>> {
    vec![
        Ok(Event::TextDelta { text: "ok".into() }),
        Ok(Event::Finish {
            reason: FinishReason::Stop,
        }),
    ]
}

/// A failure that isn't retried, so each binding is called once.
fn refused() -> Vec<Result<Event, ProviderError>> {
    vec![Err(ProviderError {
        kind: ErrorKind::ContextLength,
        status: Some(400),
    })]
}

fn server_error() -> Vec<Result<Event, ProviderError>> {
    vec![Err(ProviderError {
        kind: ErrorKind::Server,
        status: Some(500),
    })]
}

fn answered_by(outcome: &CallOutcome) -> &str {
    &outcome.result.as_ref().expect("an answer").binding
}

const ANTHROPIC: ModelFamily = ModelFamily::Anthropic;
const OPENAI: ModelFamily = ModelFamily::Openai;
const OPEN: ModelFamily = ModelFamily::OpenWeight;

#[tokio::test(start_paused = true)]
async fn fast_falls_back_to_smart_and_records_why() {
    let setup = Setup::new()
        .binding("fast", Remote, OPEN, vec![refused()])
        .binding("smart", Remote, OPEN, vec![ok()])
        .role(ModelRole::Summary, "fast")
        .fallbacks(ModelClass::Fast, &["fast", "smart"]);
    let outcome = setup
        .router()
        .call(&mut run(), request(ModelRole::Summary))
        .await;
    assert_eq!(answered_by(&outcome), "smart");
    assert_eq!(outcome.state(), RoleState::FallbackUsed);
    assert_eq!(outcome.records[0].fallback, None);
    assert_eq!(
        outcome.records[1].fallback,
        Some(Fallback {
            from: "fast".into(),
            reason: "failed",
        })
    );
}

#[tokio::test(start_paused = true)]
async fn deep_never_falls_back() {
    let setup = Setup::new()
        .binding("deep", Remote, OPEN, vec![refused()])
        .binding("deep-alt", Remote, OPEN, vec![ok()])
        .binding("smart", Remote, OPEN, vec![ok()])
        .role(ModelRole::ReviewDeep, "deep")
        .fallbacks(ModelClass::Deep, &["deep", "deep-alt"])
        .fallbacks(ModelClass::Smart, &["smart"]);
    let mut run = run();
    run.required.push(ModelRole::ReviewDeep);
    let outcome = setup
        .router()
        .call(&mut run, request(ModelRole::ReviewDeep))
        .await;
    assert!(matches!(
        outcome.result,
        Err(CallError::ProviderFailed(Some(_)))
    ));
    assert_eq!(
        (setup.calls("deep-alt"), setup.calls("smart")),
        (0, 0),
        "no silent downgrade, not even to another deep binding"
    );
}

#[tokio::test(start_paused = true)]
async fn a_benched_primary_is_skipped_with_circuit_open() {
    let three = vec![server_error(), server_error(), server_error()];
    let setup = Setup::new()
        .binding("smart", Remote, OPEN, three)
        .binding("smart-alt", Remote, OPEN, vec![ok(), ok()])
        .role(ModelRole::Review, "smart")
        .fallbacks(ModelClass::Smart, &["smart", "smart-alt"]);
    let router = setup.router();
    // Three failures bench `smart`; this call falls back after them.
    let first = router.call(&mut run(), request(ModelRole::Review)).await;
    assert_eq!(answered_by(&first), "smart-alt");
    let second = router.call(&mut run(), request(ModelRole::Review)).await;
    assert_eq!(setup.calls("smart"), 3, "benched: not called again");
    assert_eq!(answered_by(&second), "smart-alt");
    assert_eq!(second.records.len(), 1);
    assert_eq!(
        second.records[0].fallback.as_ref().unwrap().reason,
        "circuit_open"
    );
}

#[tokio::test(start_paused = true)]
async fn a_local_primary_never_falls_back_to_a_remote_binding() {
    let setup = Setup::new()
        .binding("local", Local, OPEN, vec![refused()])
        .binding("remote", Remote, OPEN, vec![ok()])
        .role(ModelRole::Review, "local")
        .fallbacks(ModelClass::Smart, &["local", "remote"]);
    let outcome = setup
        .router()
        .call(&mut run(), request(ModelRole::Review))
        .await;
    assert_eq!(setup.calls("remote"), 0);
    assert_eq!(outcome.state(), RoleState::Unavailable);
}

#[tokio::test(start_paused = true)]
async fn a_local_only_run_skips_a_remote_primary_and_says_so() {
    let setup = Setup::new()
        .binding("remote", Remote, OPEN, vec![ok()])
        .binding("local", Local, OPEN, vec![ok()])
        .role(ModelRole::Review, "remote")
        .fallbacks(ModelClass::Smart, &["remote", "local"]);
    let mut run = run();
    run.local_only = true;
    let outcome = setup
        .router()
        .call(&mut run, request(ModelRole::Review))
        .await;
    assert_eq!(setup.calls("remote"), 0);
    assert_eq!(answered_by(&outcome), "local");
    assert_eq!(
        outcome.records[0].fallback,
        Some(Fallback {
            from: "remote".into(),
            reason: "locality",
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_local_only_run_with_nothing_on_device_is_a_locality_violation() {
    let setup = Setup::new()
        .binding("remote", Remote, OPEN, vec![ok()])
        .role(ModelRole::Review, "remote");
    let mut run = run();
    run.local_only = true;
    run.required.push(ModelRole::Review);
    let outcome = setup
        .router()
        .call(&mut run, request(ModelRole::Review))
        .await;
    assert_eq!(setup.calls("remote"), 0);
    assert_eq!(
        outcome.result.unwrap_err(),
        CallError::ProviderFailed(Some(ProviderError::new(ErrorKind::LocalityViolation)))
    );
}

#[tokio::test(start_paused = true)]
async fn the_budget_stops_the_chain() {
    let setup = Setup::new()
        .binding("fast", Remote, OPEN, vec![refused()])
        .binding("smart", Remote, OPEN, vec![ok()])
        .role(ModelRole::Summary, "fast")
        .fallbacks(ModelClass::Fast, &["fast", "smart"]);
    // Room for one and a half prompts: the failed attempt is charged its input, so
    // the fallback can't reserve.
    let prompt = serde_json::to_vec(&request(ModelRole::Summary))
        .unwrap()
        .len();
    let mut run = Run::new(
        "run-1",
        RunBudget::new(BudgetLimits {
            input_tokens: gitcko_model::estimate_tokens(prompt) * 3 / 2,
            ..limits()
        }),
    );
    let outcome = setup
        .router()
        .call(&mut run, request(ModelRole::Summary))
        .await;
    assert_eq!(setup.calls("smart"), 0);
    assert_eq!(
        outcome.result.unwrap_err(),
        CallError::BudgetExhausted(Limit::InputTokens)
    );
}

fn two_families() -> Setup {
    Setup::new()
        .binding("claude", Remote, ANTHROPIC, vec![ok(), ok()])
        .binding("gpt", Remote, OPENAI, vec![ok(), ok()])
        .role(ModelRole::Review, "claude")
        .fallbacks(ModelClass::Smart, &["claude", "gpt"])
}

fn written_by(family: ModelFamily) -> AuthorFamily {
    AuthorFamily::Known {
        family,
        signal: Signal::Trailer,
    }
}

#[tokio::test(start_paused = true)]
async fn inversion_reviews_with_another_family() {
    let setup = two_families();
    let mut run = run();
    run.author = written_by(ANTHROPIC);
    let outcome = setup
        .router()
        .call(&mut run, request(ModelRole::Review))
        .await;
    assert_eq!(answered_by(&outcome), "gpt");
    assert_eq!(
        outcome.state(),
        RoleState::Succeeded,
        "a choice, not a fallback"
    );
    assert_eq!(
        outcome.records[0].inversion,
        Some(InversionDecision::Applied)
    );
    assert_eq!(setup.calls("claude"), 0);
}

#[tokio::test(start_paused = true)]
async fn inversion_records_why_it_did_nothing() {
    // Unknown author: the default route.
    let setup = two_families();
    let outcome = setup
        .router()
        .call(&mut run(), request(ModelRole::Review))
        .await;
    assert_eq!(answered_by(&outcome), "claude");
    assert_eq!(
        outcome.records[0].inversion,
        Some(InversionDecision::AuthorUnknown)
    );

    // Turned off.
    let mut setup = two_families();
    setup.inversion = false;
    let mut run_ = run();
    run_.author = written_by(ANTHROPIC);
    let outcome = setup
        .router()
        .call(&mut run_, request(ModelRole::Review))
        .await;
    assert_eq!(answered_by(&outcome), "claude");
    assert_eq!(
        outcome.records[0].inversion,
        Some(InversionDecision::Disabled)
    );

    // One family configured.
    let setup = Setup::new()
        .binding("claude", Remote, ANTHROPIC, vec![ok()])
        .binding("claude-alt", Remote, ANTHROPIC, vec![ok()])
        .role(ModelRole::Review, "claude")
        .fallbacks(ModelClass::Smart, &["claude", "claude-alt"]);
    let mut run_ = run();
    run_.author = written_by(ANTHROPIC);
    let outcome = setup
        .router()
        .call(&mut run_, request(ModelRole::Review))
        .await;
    assert_eq!(
        outcome.records[0].inversion,
        Some(InversionDecision::SingleFamily)
    );

    // Another family exists, but only off the device in a local-only run.
    let setup = Setup::new()
        .binding("local", Local, ANTHROPIC, vec![ok()])
        .binding("gpt", Remote, OPENAI, vec![ok()])
        .role(ModelRole::Review, "local")
        .fallbacks(ModelClass::Smart, &["local", "gpt"]);
    let mut run_ = run();
    run_.author = written_by(ANTHROPIC);
    run_.local_only = true;
    let outcome = setup
        .router()
        .call(&mut run_, request(ModelRole::Review))
        .await;
    assert_eq!(answered_by(&outcome), "local");
    assert_eq!(
        outcome.records[0].inversion,
        Some(InversionDecision::NoOtherFamilyAvailable)
    );
    assert_eq!(setup.calls("gpt"), 0);

    // Other roles don't invert.
    let setup = two_families().role(ModelRole::Summary, "claude");
    let mut run_ = run();
    run_.author = written_by(ANTHROPIC);
    let outcome = setup
        .router()
        .call(&mut run_, request(ModelRole::Summary))
        .await;
    assert_eq!(outcome.records[0].inversion, None);
}

#[tokio::test(start_paused = true)]
async fn the_judge_prefers_a_family_other_than_the_reviewers() {
    let setup = two_families()
        .binding("judge", Remote, OPENAI, vec![ok()])
        .binding("judge-alt", Remote, ANTHROPIC, vec![ok()])
        .role(ModelRole::Judge, "judge");
    let setup = setup.fallbacks(ModelClass::Smart, &["claude", "gpt", "judge", "judge-alt"]);
    let router = setup.router();
    let mut run = run();
    // An openai-written PR, so inversion has `claude` review it.
    run.author = written_by(OPENAI);
    let review = router.call(&mut run, request(ModelRole::Review)).await;
    assert_eq!(answered_by(&review), "claude");
    assert_eq!(run.reviewer, Some(ANTHROPIC));
    let judge = router.call(&mut run, request(ModelRole::Judge)).await;
    // The primary `judge` (openai) is already another family than the reviewer's.
    assert_eq!(answered_by(&judge), "judge");

    // Reviewer openai: the judge's own anthropic fallback goes first.
    run.reviewer = Some(OPENAI);
    let judge = router.call(&mut run, request(ModelRole::Judge)).await;
    assert_eq!(answered_by(&judge), "claude");
    assert_eq!(
        judge.state(),
        RoleState::Succeeded,
        "a preference, not a fallback"
    );
}

#[tokio::test(start_paused = true)]
async fn the_judge_uses_the_reviewers_family_when_it_is_all_there_is() {
    let setup = Setup::new()
        .binding("judge", Remote, ANTHROPIC, vec![ok()])
        .role(ModelRole::Judge, "judge");
    let mut run = run();
    run.reviewer = Some(ANTHROPIC);
    let outcome = setup
        .router()
        .call(&mut run, request(ModelRole::Judge))
        .await;
    assert_eq!(answered_by(&outcome), "judge");
}

#[test]
fn every_outcome_maps_to_exactly_one_role_state() {
    let failed = Some(ProviderError::new(ErrorKind::Server));
    let table = [
        (Err(CallError::NotConfigured), RoleState::NotConfigured),
        (
            Err(CallError::ProviderFailed(failed.clone())),
            RoleState::Unavailable,
        ),
        (Err(CallError::Unavailable(failed)), RoleState::Unavailable),
        (Err(CallError::Unavailable(None)), RoleState::Unavailable),
        (Err(CallError::Canceled), RoleState::Canceled),
        (
            Err(CallError::BudgetExhausted(Limit::Deadline)),
            RoleState::BudgetExhausted,
        ),
    ];
    for (result, state) in table {
        let outcome = CallOutcome {
            result,
            records: vec![],
        };
        assert_eq!(outcome.state(), state);
    }
}

/// One random binding: its destination, family and whether it answers.
fn arb_binding() -> impl Strategy<Value = (bool, u8, bool)> {
    (any::<bool>(), 0u8..3, any::<bool>())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Locality is a hard limit, whatever the configuration: in a local-only run, or
    /// for a role whose primary is on-device, every provider that receives a request
    /// is on-device, fallbacks and inversion included. Every attempt also fits the
    /// budget, and every attempt on a stand-in binding records why.
    #[test]
    fn no_request_ever_leaves_the_allowed_destinations(
        bindings in prop::collection::vec(arb_binding(), 1..6),
        chain in prop::collection::vec(0usize..6, 0..6),
        primary in 0usize..6,
        local_only: bool,
        author in 0u8..4,
        output_tokens in 1u64..600,
    ) {
        let families = [ANTHROPIC, OPENAI, OPEN];
        let mut setup = Setup::new();
        for (i, (local, family, answers)) in bindings.iter().enumerate() {
            let class = if *local { Local } else { Remote };
            let script = if *answers { vec![ok()] } else { vec![refused()] };
            setup = setup.binding(&format!("b{i}"), class, families[*family as usize], script);
        }
        let primary = primary % bindings.len();
        let chain: Vec<String> = chain.iter().map(|i| format!("b{}", i % bindings.len())).collect();
        setup = setup.role(ModelRole::Review, &format!("b{primary}"));
        setup.fallbacks.insert(ModelClass::Smart, chain);
        let primary_local = bindings[primary].0;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        let limits = BudgetLimits { output_tokens, ..limits() };
        let mut run = Run::new("run-1", RunBudget::new(limits));
        run.local_only = local_only;
        if let Some(family) = families.get(author as usize) {
            run.author = written_by(*family);
        }
        let outcome = runtime.block_on(setup.router().call(&mut run, request(ModelRole::Review)));

        for (i, (local, ..)) in bindings.iter().enumerate() {
            let called = setup.calls(&format!("b{i}")) > 0;
            if local_only || primary_local {
                prop_assert!(!called || *local, "b{i} is remote and was called");
            }
        }
        let spent = run.budget.spent();
        prop_assert!(spent.input_tokens <= limits.input_tokens);
        prop_assert!(spent.output_tokens <= limits.output_tokens);
        for (n, record) in outcome.records.iter().enumerate() {
            if n > 0 {
                prop_assert!(record.fallback.as_ref().is_some_and(|f| !f.reason.is_empty()));
            }
        }
    }
}

/// Output then a failure isn't retried on that binding (P3_PLAN §5): it is a fallback
/// decision. The caller only ever sees a whole response, and the record says why.
#[tokio::test(start_paused = true)]
async fn output_then_failure_falls_back_and_says_so() {
    let partial = vec![
        Ok(Event::TextDelta {
            text: "half".into(),
        }),
        Err(ProviderError::new(ErrorKind::Connect)),
    ];
    let setup = Setup::new()
        .binding("smart", Remote, OPEN, vec![partial])
        .binding("smart-alt", Remote, OPEN, vec![ok()])
        .role(ModelRole::Review, "smart")
        .fallbacks(ModelClass::Smart, &["smart", "smart-alt"]);
    let outcome = setup
        .router()
        .call(&mut run(), request(ModelRole::Review))
        .await;
    assert_eq!(setup.calls("smart"), 1);
    assert_eq!(answered_by(&outcome), "smart-alt");
    assert_eq!(outcome.result.as_ref().unwrap().text, "ok");
    assert_eq!(
        outcome.records[1].fallback.as_ref().unwrap().reason,
        "partial_output"
    );
}
