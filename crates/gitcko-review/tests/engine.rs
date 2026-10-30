// SPDX-License-Identifier: AGPL-3.0-or-later

//! The engine end to end over the synthetic review fixtures, with a scripted provider
//! standing in for the model: every disposition, every failure path, every loop bound.
//! No network and no clock: time-sensitive tests run on tokio's paused clock.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream;
use gitcko_git::Repo;
use gitcko_model::{
    BudgetLimits, Destination, DestinationClass, ErrorKind, Event, EventStream, FinishReason,
    Message, ModelProvider, ProviderError, Request, ResolvedBinding, ResolvedModels, Router, Run,
    RunBudget, ToolCall, Usage,
};
use gitcko_protocol::config::{ModelFamily, ModelProvider as Provider, ModelRole};
use gitcko_protocol::event::RunEventKind;
use gitcko_protocol::review::{ReviewStatus, RiskLevel, RiskSource, ScoreAvailabilityReason};
use gitcko_review::{Outcome, ReviewInput, base_budget, review};
use gitcko_test_support::{TempDir, script_repo, workspace_root};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

type Reply = Vec<Result<Event, ProviderError>>;

type Answer = dyn Fn(&Request, &CancellationToken) -> Reply + Send + Sync;

/// A stand-in model: answers each request from its contents, and keeps them all.
struct Scripted {
    reply: Box<Answer>,
    requests: Mutex<Vec<Request>>,
    delay: Duration,
    /// Runs as the reply's last event is delivered.
    at_finish: Arc<dyn Fn(&Request) + Send + Sync>,
}

impl ModelProvider for Scripted {
    fn stream(&self, request: Request, cancel: CancellationToken) -> EventStream {
        let events = (self.reply)(&request, &cancel);
        let at_finish = self.at_finish.clone();
        self.requests.lock().unwrap().push(request.clone());
        let delay = self.delay;
        Box::pin(stream::iter(events).then(move |e| {
            let (at_finish, request) = (at_finish.clone(), request.clone());
            async move {
                tokio::time::sleep(delay).await;
                if matches!(e, Ok(Event::Finish { .. })) {
                    at_finish(&request);
                }
                e
            }
        }))
    }
}

fn text(body: &str) -> Reply {
    text_costing(body, 100)
}

fn text_costing(body: &str, input_tokens: u64) -> Reply {
    vec![
        Ok(Event::TextDelta { text: body.into() }),
        Ok(Event::Usage(Usage {
            input_tokens: Some(input_tokens),
            output_tokens: Some(10),
            cost_usd: None,
        })),
        Ok(Event::Finish {
            reason: FinishReason::Stop,
        }),
    ]
}

fn fail(kind: ErrorKind) -> Reply {
    vec![Err(ProviderError::new(kind))]
}

fn tools(calls: &[(&str, &str, Value)]) -> Reply {
    let mut out: Reply = calls
        .iter()
        .map(|(id, name, args)| {
            Ok(Event::ToolCall(ToolCall {
                id: (*id).into(),
                name: (*name).into(),
                arguments: args.to_string(),
            }))
        })
        .collect();
    out.push(Ok(Event::Finish {
        reason: FinishReason::ToolCalls,
    }));
    out
}

fn risk_ok() -> String {
    json!({"level": "medium", "reason": "a change", "priorities": []}).to_string()
}

fn hypothesis(path: &str, title: &str) -> Value {
    json!({"title": title, "category": "correctness", "path": path, "start_line": 1,
           "end_line": 3, "claim": "It may be wrong."})
}

fn plan_of(hypotheses: &[Value]) -> String {
    json!({ "hypotheses": hypotheses }).to_string()
}

fn refuted() -> String {
    json!({"outcome": "refuted", "reason": "It holds.", "candidates": []}).to_string()
}

fn candidate(path: &str, evidence: &[&str]) -> Value {
    json!({"title": "Divides by zero", "category": "correctness", "severity": "P1",
           "comment_type": "logic", "confidence": 0.9, "path": path, "side": "new",
           "start_line": 1, "end_line": 3, "trigger": "An empty slice.",
           "cause": "No guard.", "impact": "It panics.", "evidence": evidence,
           "counterevidence": [], "suggestion": null})
}

fn supported(candidates: &[Value]) -> String {
    json!({"outcome": "supported", "reason": "Confirmed.", "candidates": candidates}).to_string()
}

/// What a request is for. A repair keeps the schema of the call it repairs.
fn step(request: &Request) -> &str {
    match &request.response_format {
        Some(format) if format.name == "investigation" => "investigate",
        Some(format) => format.name.as_str(),
        None => "",
    }
}

/// What the happy path replies with for each prompt, for tests to override.
fn default_reply(request: &Request) -> Reply {
    match step(request) {
        "risk" => text(&risk_ok()),
        "plan" => text(&plan_of(&[hypothesis(
            "src/lib.rs",
            "Is the average right",
        )])),
        _ => text(&refuted()),
    }
}

fn scripted(
    reply: impl Fn(&Request, &CancellationToken) -> Reply + Send + Sync + 'static,
) -> Arc<Scripted> {
    Arc::new(Scripted {
        reply: Box::new(reply),
        requests: Mutex::new(Vec::new()),
        delay: Duration::ZERO,
        at_finish: Arc::new(|_| {}),
    })
}

fn router(provider: Arc<dyn ModelProvider>) -> Router {
    let binding = ResolvedBinding {
        name: "smart".into(),
        provider: Provider::Openrouter,
        model: "fixture/smart".into(),
        family: ModelFamily::OpenWeight,
        reasoning_effort: None,
        data_policy: None,
        endpoint: None,
        context: None,
        destination: Destination {
            class: DestinationClass::CustomerRemote,
            host: "openrouter.ai".into(),
            port: 443,
        },
        credential: None,
    };
    let models = ResolvedModels {
        bindings: BTreeMap::from([("smart".to_string(), binding)]),
        roles: BTreeMap::from([
            (ModelRole::Review, "smart".to_string()),
            (ModelRole::RiskClassifier, "smart".to_string()),
        ]),
        fallbacks: BTreeMap::new(),
        inversion: false,
    };
    Router::new(models, BTreeMap::from([("smart".to_string(), provider)]))
}

fn case_dir(suite: &str, case: &str) -> PathBuf {
    workspace_root()
        .join("tests/fixtures/review")
        .join(suite)
        .join(case)
}

fn fixture(suite: &str, case: &str) -> TempDir {
    script_repo(&case_dir(suite, case).join("make.sh"), case)
}

fn run_with_budget(limits: BudgetLimits) -> Run {
    let mut run = Run::new("rv_test".to_string(), RunBudget::new(limits));
    run.required = vec![ModelRole::Review];
    run
}

fn at_rev(repo: &Repo, rev: &str) -> gitcko_git::CommitId {
    repo.resolve(rev).unwrap()
}

async fn review_repo(dir: &Path, provider: Arc<Scripted>, run: Run) -> Outcome {
    let repo = Repo::open(dir).unwrap();
    let now = || "2026-01-01T00:00:00Z".to_string();
    let input = ReviewInput {
        repo: &repo,
        base: at_rev(&repo, "base"),
        head: at_rev(&repo, "main"),
        now: &now,
    };
    review(input, &router(provider), run).await
}

async fn review_case(suite: &str, case: &str, provider: Arc<Scripted>) -> Outcome {
    let dir = fixture(suite, case);
    review_repo(dir.path(), provider, run_with_budget(base_budget())).await
}

/// `fault_crate`, the change every Suite F case reviews.
async fn review_fault_crate(provider: Arc<Scripted>, run: Run) -> Outcome {
    let dir = fixture("f", "timeout_plan");
    review_repo(dir.path(), provider, run).await
}

fn codes(outcome: &Outcome) -> Vec<&str> {
    outcome
        .review
        .reason_codes
        .iter()
        .map(String::as_str)
        .collect()
}

fn step_count(outcome: &Outcome, key: &str) -> u64 {
    outcome
        .events
        .iter()
        .find_map(|e| match &e.kind {
            RunEventKind::StepCompleted { step, counts, .. } if step == "investigate" => {
                counts.get(key).copied()
            }
            _ => None,
        })
        .unwrap_or(0)
}

fn all_prompt_text(requests: &[Request]) -> String {
    let mut out = String::new();
    for request in requests {
        for message in &request.messages {
            match message {
                Message::System { text } | Message::User { text } => out.push_str(text),
                Message::Assistant { text, .. } => out.push_str(text),
                Message::Tool { text, .. } => out.push_str(text),
            }
            out.push('\n');
        }
    }
    out
}

// ---- Suite P: what the review could see decides the result ----

#[tokio::test]
async fn every_p_case_reports_the_expected_status_and_reasons() {
    let root = workspace_root().join("tests/fixtures/review/p");
    let mut cases: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    cases.sort();
    assert!(cases.len() >= 20, "the suite went missing: {cases:?}");
    let mut wrong = Vec::new();
    for case in cases {
        let expect: toml::Value =
            toml::from_str(&std::fs::read_to_string(root.join(&case).join("expect.toml")).unwrap())
                .unwrap();
        let want_status = expect["status"].as_str().unwrap().to_string();
        let want_codes: Vec<String> = expect
            .get("reason_codes")
            .and_then(toml::Value::as_array)
            .map(|c| c.iter().map(|v| v.as_str().unwrap().to_string()).collect())
            .unwrap_or_default();
        let provider = scripted(|r, _| match step(r) {
            "plan" => text(&plan_of(&[])),
            _ => default_reply(r),
        });
        let outcome = review_case("p", &case, provider).await;
        let got_status = serde_json::to_value(outcome.review.status)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        if got_status != want_status || outcome.review.reason_codes != want_codes {
            wrong.push((
                case,
                got_status,
                outcome.review.reason_codes.clone(),
                want_status,
                want_codes,
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[tokio::test]
async fn a_held_back_file_never_reaches_a_prompt_even_when_the_model_asks() {
    let provider = scripted(|r, _| match step(r) {
        "plan" => text(&plan_of(&[hypothesis(".env", "Is the env wrong")])),
        "investigate" => text(&supported(&[candidate(".env", &["e1"])])),
        _ => default_reply(r),
    });
    let outcome = review_case("p", "held_back_env", provider.clone()).await;
    assert_eq!(outcome.review.status, ReviewStatus::Partial);
    assert_eq!(codes(&outcome), ["sensitive_files_held_back"]);
    assert_eq!(outcome.review.coverage.held_back, [".env"]);
    assert!(outcome.review.findings.is_empty());
    let requests = provider.requests.lock().unwrap();
    // There is nothing to send: the only changed file is held back.
    assert!(requests.is_empty(), "{} calls", requests.len());
}

#[tokio::test]
async fn a_changed_file_beside_a_held_back_one_is_still_reviewed() {
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review_case("p", "binary_beside_rust", provider.clone()).await;
    assert_eq!(outcome.review.status, ReviewStatus::Complete);
    assert_eq!(outcome.review.coverage.skipped_binary.len(), 1);
    assert_eq!(outcome.review.coverage.investigated_files, 1);
    assert!(all_prompt_text(&provider.requests.lock().unwrap()).contains("src/lib.rs"));
}

#[tokio::test]
async fn a_rule_added_on_the_head_does_not_apply_to_its_own_review() {
    // The head adds `.gitcko/config.toml` ignoring `src/`; only the base's config counts.
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review_case("adv-a", "config_ignore_src", provider.clone()).await;
    // The source and the new config file; with the head's config it would be one.
    assert_eq!(outcome.review.coverage.investigated_files, 2);
    assert!(all_prompt_text(&provider.requests.lock().unwrap()).contains("src/lib.rs"));
}

#[tokio::test]
async fn a_valid_ignore_added_on_the_head_is_not_obeyed() {
    let scripts = TempDir::new("head-config");
    let script = scripts.path().join("make.sh");
    let body = r#"#!/usr/bin/env bash
set -euo pipefail
git init -q -b main
mkdir -p src .gitcko
echo 'pub fn one() -> u32 { 1 }' > src/lib.rs
git add -A
git commit -q -m Base
git branch base
echo 'pub fn one() -> u32 { 2 }' > src/lib.rs
printf 'schema_version = 1\nignore = ["src/**"]\n' > .gitcko/config.toml
git add -A
git commit -q -m Head
"#;
    std::fs::write(&script, body).unwrap();
    let dir = script_repo(&script, "head-config");
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review_repo(dir.path(), provider, run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.status, ReviewStatus::Complete);
    assert_eq!(outcome.review.coverage.investigated_files, 2);
}

#[tokio::test]
async fn a_hypothesis_about_a_file_outside_the_review_is_not_investigated() {
    let provider = scripted(|r, _| match step(r) {
        "plan" => text(&plan_of(&[
            hypothesis("src/elsewhere.rs", "Outside"),
            hypothesis("src/lib.rs", "Inside"),
        ])),
        _ => default_reply(r),
    });
    let outcome = review_fault_crate(provider.clone(), run_with_budget(base_budget())).await;
    let cleared: Vec<&str> = outcome
        .review
        .cleared_risks
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(cleared, ["Inside"]);
}

/// A repository with `.gitcko/` files on the base and a change to two files.
fn with_base_config(config: &str) -> TempDir {
    let scripts = TempDir::new("base-config");
    let script = scripts.path().join("make.sh");
    let body = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
git init -q -b main
mkdir -p src gen .gitcko
echo 'pub fn one() -> u32 {{ 1 }}' > src/lib.rs
echo 'pub fn gen() -> u32 {{ 1 }}' > gen/out.rs
cat > .gitcko/config.toml <<'GITCKO'
{config}
GITCKO
echo 'Always look for the frobnicator.' > .gitcko/rules.md
git add -A
git commit -q -m Base
git branch base
echo 'pub fn one() -> u32 {{ 2 }}' > src/lib.rs
echo 'pub fn gen() -> u32 {{ 2 }}' > gen/out.rs
git add -A
git commit -q -m Head
"#
    );
    std::fs::write(&script, body).unwrap();
    script_repo(&script, "base-config")
}

#[tokio::test]
async fn the_base_commit_s_ignore_and_rules_apply() {
    let dir = with_base_config("schema_version = 1\nignore = [\"gen/**\"]");
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review_repo(dir.path(), provider.clone(), run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.reason_codes, Vec::<String>::new());
    assert_eq!(outcome.review.status, ReviewStatus::Complete);
    assert_eq!(outcome.review.coverage.changed_files, 2);
    assert_eq!(outcome.review.coverage.investigated_files, 1);
    let sent = all_prompt_text(&provider.requests.lock().unwrap());
    assert!(sent.contains("src/lib.rs"));
    assert!(!sent.contains("gen/out.rs"));
    assert!(sent.contains("frobnicator"));
}

#[tokio::test]
async fn broken_config_on_the_base_is_reported_and_the_review_goes_on() {
    let dir = with_base_config("schema_version = 1\nignore = [");
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review_repo(dir.path(), provider, run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.status, ReviewStatus::Partial);
    assert_eq!(codes(&outcome), ["config_invalid"]);
    assert_eq!(outcome.review.coverage.investigated_files, 2);
}

fn repo_from(label: &str, body: &str) -> TempDir {
    let scripts = TempDir::new(label);
    let script = scripts.path().join("make.sh");
    std::fs::write(&script, body).unwrap();
    script_repo(&script, label)
}

const TWO_FOLDERS: &str = r#"#!/usr/bin/env bash
set -euo pipefail
git init -q -b main
mkdir -p a/.gitcko b/.gitcko
echo 'pub fn a() -> u32 { 1 }' > a/x.rs
echo 'pub fn b() -> u32 { 1 }' > b/y.rs
echo 'Rule of folder alpha.' > a/.gitcko/rules.md
echo 'Rule of folder beta.' > b/.gitcko/rules.md
BROKEN
git add -A
git commit -q -m Base
git branch base
echo 'pub fn a() -> u32 { 2 }' > a/x.rs
echo 'pub fn b() -> u32 { 2 }' > b/y.rs
git add -A
git commit -q -m Head
"#;

#[tokio::test]
async fn each_folder_s_rules_reach_the_model_with_their_scope() {
    let dir = repo_from("two-folders", &TWO_FOLDERS.replace("BROKEN", ""));
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review_repo(dir.path(), provider.clone(), run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.status, ReviewStatus::Complete);
    let sent = all_prompt_text(&provider.requests.lock().unwrap());
    assert!(sent.contains("Rule of folder alpha."), "{sent}");
    assert!(sent.contains("Rule of folder beta."), "{sent}");
    assert!(sent.contains("Applies only to: a/x.rs"), "{sent}");
    assert!(sent.contains("Applies only to: b/y.rs"), "{sent}");
}

#[tokio::test]
async fn a_broken_nested_config_is_reported_though_its_parent_stands_in() {
    let broken = "echo 'not = [valid' > b/.gitcko/config.toml";
    let dir = repo_from("broken-nested", &TWO_FOLDERS.replace("BROKEN", broken));
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review_repo(dir.path(), provider, run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.status, ReviewStatus::Partial);
    assert_eq!(codes(&outcome), ["config_invalid"]);
}

#[tokio::test]
async fn a_secret_renamed_to_an_innocent_name_stays_out_of_every_tool() {
    let dir = repo_from(
        "renamed-secret",
        r#"#!/usr/bin/env bash
set -euo pipefail
git init -q -b main
mkdir -p src
echo 'pub fn one() -> u32 { 1 }' > src/lib.rs
echo '{"password": "opensesame-marker"}' > credentials.json
git add -A
git commit -q -m Base
git branch base
echo 'pub fn one() -> u32 { 2 }' > src/lib.rs
git mv credentials.json config.json
git add -A
git commit -q -m Head
"#,
    );
    let provider = scripted(|r, _| {
        if step(r) != "investigate" {
            return default_reply(r);
        }
        if !was_answered(r) {
            return tools(&[
                ("c1", "read_file", json!({"path": "config.json"})),
                (
                    "c2",
                    "read_file",
                    json!({"path": "credentials.json", "side": "old"}),
                ),
                ("c3", "read_diff", json!({"path": "config.json"})),
                ("c4", "search_code", json!({"query": "opensesame"})),
                ("c5", "read_file", read_lib()),
            ]);
        }
        text(&refuted())
    });
    let outcome = review_repo(dir.path(), provider.clone(), run_with_budget(base_budget())).await;
    assert_eq!(codes(&outcome), ["sensitive_files_held_back"]);
    let requests = provider.requests.lock().unwrap();
    let last = requests.last().unwrap();
    let results: Vec<Value> = last
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::Tool { text, .. } => tool_json(text),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 5);
    assert_eq!(results[0]["status"], "held_back", "{}", results[0]);
    assert_eq!(results[1]["status"], "held_back", "{}", results[1]);
    assert_eq!(results[2]["status"], "held_back", "{}", results[2]);
    // Positive control: the other changed file reads fine.
    assert_eq!(results[4]["status"], "ok", "{}", results[4]);
    assert!(!all_prompt_text(&requests).contains("opensesame-marker"));
}

#[tokio::test]
async fn a_key_pasted_into_a_commit_subject_is_scrubbed_before_any_prompt() {
    let key = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
    let dir = repo_from(
        "subject-key",
        &format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
git init -q -b main
mkdir -p src
echo 'pub fn one() -> u32 {{ 1 }}' > src/lib.rs
git add -A
git commit -q -m Base
git branch base
echo 'pub fn one() -> u32 {{ 2 }}' > src/lib.rs
git add -A
git commit -q -m "Bump with subject-marker {key}"
"#
        ),
    );
    let provider = scripted(|r, _| default_reply(r));
    review_repo(dir.path(), provider.clone(), run_with_budget(base_budget())).await;
    let sent = all_prompt_text(&provider.requests.lock().unwrap());
    // Positive control: the subject itself did go out.
    assert!(sent.contains("subject-marker"), "{sent}");
    assert!(!sent.contains(&key));
}

// ---- Suite F: every failure is on the result ----

#[tokio::test]
async fn a_failed_optional_risk_call_falls_back_to_rules_and_stays_complete() {
    for kind in [
        ErrorKind::Auth,
        ErrorKind::BadRequest,
        ErrorKind::ContentPolicy,
    ] {
        let provider = scripted(move |r, _| match step(r) {
            "risk" => fail(kind.clone()),
            _ => default_reply(r),
        });
        let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
        assert_eq!(outcome.review.status, ReviewStatus::Complete);
        assert!(outcome.review.reason_codes.is_empty());
        let risk = outcome.review.risk_level.as_ref().unwrap();
        assert_eq!(risk.source, RiskSource::Rules);
    }
    // Positive control: a working classifier is the source when it answers.
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    let risk = outcome.review.risk_level.as_ref().unwrap();
    assert_eq!(risk.source, RiskSource::Model);
}

#[tokio::test]
async fn a_provider_failure_at_planning_leaves_a_failed_result() {
    let provider = scripted(|r, _| match step(r) {
        "plan" => fail(ErrorKind::Auth),
        _ => default_reply(r),
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.status, ReviewStatus::Failed);
    assert_eq!(codes(&outcome), ["provider_failure", "review_failed"]);
    assert_eq!(
        outcome.review.score_availability_reason,
        Some(ScoreAvailabilityReason::Failed)
    );
    assert!(outcome.review.score.is_none());
}

#[tokio::test]
async fn a_provider_failure_while_investigating_is_partial() {
    let provider = scripted(|r, _| match step(r) {
        "investigate" => fail(ErrorKind::Auth),
        _ => default_reply(r),
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.status, ReviewStatus::Partial);
    assert_eq!(codes(&outcome), ["provider_failure", "review_failed"]);
    assert_eq!(
        outcome.review.score_availability_reason,
        Some(ScoreAvailabilityReason::Incomplete)
    );
}

#[tokio::test]
async fn a_refusal_at_any_required_step_is_partial_and_stops() {
    for prompt in ["plan", "investigate"] {
        let provider = scripted(move |r, _| {
            if step(r) == prompt {
                fail(ErrorKind::ContentPolicy)
            } else {
                default_reply(r)
            }
        });
        let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
        assert_eq!(outcome.review.status, ReviewStatus::Partial, "{prompt}");
        assert_eq!(codes(&outcome), ["model_refused"], "{prompt}");
    }
    // The safety system can also end a reply that had already started.
    let provider = scripted(|r, _| match step(r) {
        "investigate" => vec![Ok(Event::Finish {
            reason: FinishReason::ContentFilter,
        })],
        _ => default_reply(r),
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert_eq!(codes(&outcome), ["model_refused"]);
}

#[tokio::test]
async fn bad_output_gets_one_repair_and_then_a_recorded_drop() {
    for prompt in ["plan", "investigate"] {
        let seen = Arc::new(Mutex::new(0));
        let counter = seen.clone();
        let provider = scripted(move |r, _| {
            if step(r) != prompt {
                return default_reply(r);
            }
            *counter.lock().unwrap() += 1;
            text("not json at all")
        });
        let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
        assert_eq!(outcome.review.status, ReviewStatus::Partial, "{prompt}");
        assert_eq!(codes(&outcome), ["invalid_model_output"], "{prompt}");
        assert_eq!(
            *seen.lock().unwrap(),
            2,
            "{prompt}: first try plus one repair"
        );
    }
}

#[tokio::test]
async fn a_good_repair_leaves_no_mark() {
    for prompt in ["risk", "plan", "investigate"] {
        let seen = Arc::new(Mutex::new(0));
        let counter = seen.clone();
        let provider = scripted(move |r, _| {
            if step(r) != prompt {
                return default_reply(r);
            }
            let mut n = counter.lock().unwrap();
            *n += 1;
            if *n == 1 {
                text("{ broken")
            } else {
                default_reply(r)
            }
        });
        let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
        assert_eq!(outcome.review.status, ReviewStatus::Complete, "{prompt}");
        assert_eq!(*seen.lock().unwrap(), 2, "{prompt}");
    }
}

#[tokio::test]
async fn hostile_output_never_panics() {
    let hostile = [
        "",
        "null",
        "[]",
        "{\"outcome\":\"supported\"}",
        "\u{0}\u{0}",
        "{\"outcome\":\"supported\",\"reason\":\"x\",\"candidates\":[{}]}",
        &format!("{{\"hypotheses\":[{}]}}", "{},".repeat(4000) + "{}"),
    ];
    for body in hostile {
        let body = body.to_string();
        let provider = scripted(move |_, _| text(&body));
        let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
        assert_ne!(outcome.review.status, ReviewStatus::Complete);
        assert!(outcome.review.findings.is_empty());
    }
}

#[tokio::test]
async fn cancel_at_any_step_says_canceled_and_makes_no_more_calls() {
    for prompt in ["risk", "plan", "investigate"] {
        let run = run_with_budget(base_budget());
        let cancel = run.cancel.clone();
        let provider = scripted(move |r, _| {
            if step(r) == prompt {
                cancel.cancel();
            }
            default_reply(r)
        });
        let outcome = review_fault_crate(provider.clone(), run).await;
        assert_eq!(outcome.review.status, ReviewStatus::Canceled, "{prompt}");
        assert_eq!(codes(&outcome), ["canceled"], "{prompt}");
        assert_eq!(
            outcome.review.score_availability_reason,
            Some(ScoreAvailabilityReason::Canceled)
        );
        let requests = provider.requests.lock().unwrap();
        assert_eq!(
            step(requests.last().unwrap()),
            prompt,
            "nothing after the cancel"
        );
    }
}

#[tokio::test]
async fn canceled_before_the_first_call_sends_nothing() {
    let provider = scripted(|r, _| default_reply(r));
    let run = run_with_budget(base_budget());
    run.cancel.cancel();
    let outcome = review_fault_crate(provider.clone(), run).await;
    assert_eq!(outcome.review.status, ReviewStatus::Canceled);
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn running_out_of_tokens_is_partial_with_the_limit_named() {
    let provider = scripted(|r, _| default_reply(r));
    let limits = BudgetLimits {
        input_tokens: 50,
        output_tokens: 40_000,
        cost_usd: None,
    };
    let outcome = review_fault_crate(provider, run_with_budget(limits)).await;
    assert_eq!(outcome.review.status, ReviewStatus::Partial);
    assert_eq!(codes(&outcome), ["budget_exhausted", "tokens"]);
}

#[tokio::test]
async fn investigation_stops_at_the_reserve_and_says_so() {
    // Each call reports 10k input tokens. At a limit of 40k the reserve starts at 30k:
    // risk and plan spend 20k, the first investigation reaches it, the rest wait.
    const CALL: u64 = 10_000;
    let provider = scripted(|r, _| match step(r) {
        "risk" => text_costing(&risk_ok(), CALL),
        "plan" => text_costing(
            &plan_of(&[
                hypothesis("src/lib.rs", "First"),
                hypothesis("src/lib.rs", "Second"),
                hypothesis("src/lib.rs", "Third"),
            ]),
            CALL,
        ),
        _ => text_costing(&refuted(), CALL),
    });
    let limits = BudgetLimits {
        input_tokens: 4 * CALL,
        output_tokens: 40_000,
        cost_usd: None,
    };
    let outcome = review_fault_crate(provider.clone(), run_with_budget(limits)).await;
    assert_eq!(codes(&outcome), ["budget_exhausted", "tokens"]);
    assert_eq!(step_count(&outcome, "deferred"), 2);
    let investigations = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| step(r) == "investigate")
        .count();
    assert_eq!(investigations, 1);
}

#[tokio::test(start_paused = true)]
async fn the_deadline_is_partial_with_the_limit_named() {
    let provider = Arc::new(Scripted {
        reply: Box::new(|r, _| default_reply(r)),
        requests: Mutex::new(Vec::new()),
        delay: Duration::from_secs(10),
        at_finish: Arc::new(|_| {}),
    });
    let mut run = run_with_budget(base_budget());
    run.deadline = Some(tokio::time::Instant::now() + Duration::from_secs(1));
    let outcome = review_fault_crate(provider, run).await;
    assert_eq!(outcome.review.status, ReviewStatus::Partial);
    assert_eq!(codes(&outcome), ["budget_exhausted", "deadline"]);
}

#[tokio::test]
async fn a_review_that_cannot_read_the_repository_fails() {
    let dir = fixture("f", "timeout_plan");
    let repo = Repo::open(dir.path()).unwrap();
    let head = at_rev(&repo, "main");
    // A commit from another repository: a valid id this one doesn't hold.
    let other = fixture("p", "binary_only");
    let missing = at_rev(&Repo::open(other.path()).unwrap(), "main");
    let now = || "2026-01-01T00:00:00Z".to_string();
    let input = ReviewInput {
        repo: &repo,
        base: missing,
        head,
        now: &now,
    };
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review(
        input,
        &router(provider.clone()),
        run_with_budget(base_budget()),
    )
    .await;
    assert_eq!(outcome.review.status, ReviewStatus::Failed);
    assert_eq!(codes(&outcome), ["snapshot_failed"]);
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_cancel_arriving_with_the_reply_runs_no_tool() {
    let run = run_with_budget(base_budget());
    let cancel = run.cancel.clone();
    let provider = Arc::new(Scripted {
        reply: Box::new(|r, _| match step(r) {
            "investigate" => tools(&[("c1", "read_file", read_lib())]),
            _ => default_reply(r),
        }),
        requests: Mutex::new(Vec::new()),
        delay: Duration::ZERO,
        at_finish: Arc::new(move |r| {
            if step(r) == "investigate" {
                cancel.cancel();
            }
        }),
    });
    let outcome = review_fault_crate(provider, run).await;
    assert_eq!(outcome.review.status, ReviewStatus::Canceled);
    assert_eq!(step_count(&outcome, "tool_actions"), 0);
}

// ---- The investigator loop ----

fn read_lib() -> Value {
    json!({"path": "src/lib.rs"})
}

/// A tool result's JSON, out of its fence.
fn tool_json(fenced: &str) -> Option<Value> {
    let mut lines: Vec<&str> = fenced.lines().skip(1).collect();
    lines.pop();
    serde_json::from_str(&lines.join("\n")).ok()
}

/// The evidence ids tools have handed back so far.
fn evidence_ids(request: &Request) -> Vec<String> {
    request
        .messages
        .iter()
        .filter_map(|m| match m {
            Message::Tool { text, .. } => tool_json(text),
            _ => None,
        })
        .filter_map(|v| v["evidence"].as_str().map(str::to_string))
        .collect()
}

fn was_answered(request: &Request) -> bool {
    request
        .messages
        .iter()
        .any(|m| matches!(m, Message::Tool { .. }))
}

#[tokio::test]
async fn a_supported_candidate_citing_real_evidence_is_published() {
    let provider = scripted(|r, _| {
        if step(r) != "investigate" {
            return default_reply(r);
        }
        if !was_answered(r) {
            return tools(&[("c1", "read_file", read_lib())]);
        }
        let ids = evidence_ids(r);
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        text(&supported(&[candidate("src/lib.rs", &ids)]))
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.status, ReviewStatus::Complete);
    let [finding] = outcome.review.findings.as_slice() else {
        panic!("{:?}", outcome.review.findings);
    };
    assert_eq!(finding.path, "src/lib.rs");
    assert!(finding.id.starts_with("f_"), "{}", finding.id);
    assert_eq!(finding.id.len(), 18, "{}", finding.id);
    assert_eq!(finding.evidence.len(), 1);
    assert_eq!(finding.evidence[0].path, "src/lib.rs");
    assert!(finding.evidence[0].start_line >= 1);
    assert!(finding.body.contains("No guard."));
    assert!(outcome.review.score.is_none());
    assert_eq!(
        outcome.review.score_availability_reason,
        Some(ScoreAvailabilityReason::GateClosed)
    );
    assert_eq!(step_count(&outcome, "tool_actions"), 1);
}

#[tokio::test]
async fn a_candidate_citing_evidence_no_tool_returned_is_dropped() {
    let provider = scripted(|r, _| match step(r) {
        "investigate" => text(&supported(&[candidate("src/lib.rs", &["e999"])])),
        _ => default_reply(r),
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert!(outcome.review.findings.is_empty());
    assert_eq!(step_count(&outcome, "dropped"), 1);
}

#[tokio::test]
async fn a_candidate_citing_no_evidence_at_all_never_parses() {
    let provider = scripted(|r, _| match step(r) {
        "investigate" => text(&supported(&[candidate("src/lib.rs", &[])])),
        _ => default_reply(r),
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert!(outcome.review.findings.is_empty());
    assert_eq!(codes(&outcome), ["invalid_model_output"]);
}

#[tokio::test]
async fn a_candidate_about_a_file_outside_the_review_is_dropped() {
    let provider = scripted(|r, _| {
        if step(r) != "investigate" {
            return default_reply(r);
        }
        if !was_answered(r) {
            return tools(&[("c1", "read_file", read_lib())]);
        }
        let ids = evidence_ids(r);
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        text(&supported(&[candidate("elsewhere.rs", &ids)]))
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert!(outcome.review.findings.is_empty());
    assert_eq!(step_count(&outcome, "dropped"), 1);
}

#[tokio::test]
async fn a_refuted_hypothesis_becomes_a_cleared_risk() {
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    let [cleared] = outcome.review.cleared_risks.as_slice() else {
        panic!("{:?}", outcome.review.cleared_risks);
    };
    assert_eq!(cleared.name, "Is the average right");
    assert_eq!(cleared.description, "It holds.");
}

#[tokio::test]
async fn repeating_the_same_tool_call_ends_as_inconclusive() {
    let provider = scripted(|r, _| match step(r) {
        "investigate" => tools(&[("c1", "read_file", read_lib())]),
        _ => default_reply(r),
    });
    let outcome = review_fault_crate(provider.clone(), run_with_budget(base_budget())).await;
    assert_eq!(step_count(&outcome, "inconclusive"), 1);
    assert_eq!(step_count(&outcome, "tool_actions"), 1);
    assert_eq!(outcome.review.status, ReviewStatus::Partial);
    assert_eq!(codes(&outcome), ["reduced_context", "truncated"]);
    let investigate_calls = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| step(r) == "investigate")
        .count();
    // One that did something, then three that didn't.
    assert_eq!(investigate_calls, 4);
}

/// Asks for a call it has never made before, every turn.
fn always_new_call(r: &Request) -> Reply {
    let turn = r.messages.len();
    tools(&[(
        "c",
        "read_file",
        json!({"path": "src/lib.rs", "start_line": turn, "end_line": turn}),
    )])
}

#[tokio::test]
async fn the_per_hypothesis_action_cap_takes_the_tools_away() {
    let provider = scripted(|r, _| match step(r) {
        "investigate" if r.tools.is_empty() => text(&refuted()),
        "investigate" => always_new_call(r),
        _ => default_reply(r),
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert_eq!(step_count(&outcome, "tool_actions"), 12);
    assert_eq!(outcome.review.cleared_risks.len(), 1);
    assert_eq!(outcome.review.status, ReviewStatus::Complete);
}

#[tokio::test]
async fn a_model_that_keeps_calling_without_tools_is_stopped() {
    let provider = scripted(|r, _| match step(r) {
        "investigate" => always_new_call(r),
        _ => default_reply(r),
    });
    let outcome = review_fault_crate(provider.clone(), run_with_budget(base_budget())).await;
    assert_eq!(step_count(&outcome, "tool_actions"), 12);
    assert_eq!(step_count(&outcome, "inconclusive"), 1);
    let calls = provider
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| step(r) == "investigate")
        .count();
    // Twelve that ran a tool, and the one that had none left and called anyway.
    assert_eq!(calls, 13);
    assert_eq!(codes(&outcome), ["reduced_context", "truncated"]);
}

#[tokio::test]
async fn one_reply_cannot_buy_more_actions_than_the_cap() {
    let provider = scripted(|r, _| match step(r) {
        "investigate" if r.tools.is_empty() => text(&refuted()),
        "investigate" => {
            let calls: Vec<(String, Value)> = (1..=50)
                .map(|n| {
                    (
                        format!("c{n}"),
                        json!({"path": "src/lib.rs", "start_line": n, "end_line": n}),
                    )
                })
                .collect();
            let calls: Vec<(&str, &str, Value)> = calls
                .iter()
                .map(|(id, args)| (id.as_str(), "read_file", args.clone()))
                .collect();
            tools(&calls)
        }
        _ => default_reply(r),
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert_eq!(step_count(&outcome, "tool_actions"), 12);
}

#[tokio::test]
async fn the_run_action_cap_defers_the_rest_as_truncated() {
    let provider = scripted(|r, _| match step(r) {
        "plan" => {
            let many: Vec<Value> = (1..=8)
                .map(|n| hypothesis("src/lib.rs", &format!("Hypothesis {n}")))
                .collect();
            text(&plan_of(&many))
        }
        "investigate" if r.tools.is_empty() => text(&refuted()),
        "investigate" => always_new_call(r),
        _ => default_reply(r),
    });
    // Roomy enough that the action cap, not spend, is what stops it.
    let limits = BudgetLimits {
        input_tokens: 100_000_000,
        output_tokens: 40_000,
        cost_usd: None,
    };
    let outcome = review_fault_crate(provider, run_with_budget(limits)).await;
    // Twelve each for six, eight for the seventh, none left for the eighth.
    assert_eq!(step_count(&outcome, "tool_actions"), 80);
    assert_eq!(step_count(&outcome, "deferred"), 1);
    assert_eq!(codes(&outcome), ["reduced_context", "truncated"]);
    assert_eq!(outcome.review.status, ReviewStatus::Partial);
}

#[tokio::test]
async fn an_unknown_tool_runs_nothing_and_the_refusal_reaches_the_model() {
    let provider = scripted(|r, _| {
        if step(r) != "investigate" {
            return default_reply(r);
        }
        if !was_answered(r) {
            return tools(&[("c1", "run_command", json!({"command": "id"}))]);
        }
        text(&refuted())
    });
    let outcome = review_fault_crate(provider.clone(), run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.status, ReviewStatus::Complete);
    let requests = provider.requests.lock().unwrap();
    let last = requests.last().unwrap();
    let Some(Message::Tool { text, .. }) = last.messages.last() else {
        panic!("no tool result");
    };
    let result = tool_json(text).unwrap();
    assert_ne!(result["status"], "ok", "{result}");
}

#[tokio::test]
async fn only_the_three_roles_are_called() {
    let provider = scripted(|r, _| default_reply(r));
    review_fault_crate(provider.clone(), run_with_budget(base_budget())).await;
    let requests = provider.requests.lock().unwrap();
    let ids: Vec<&str> = requests.iter().map(step).collect();
    assert_eq!(ids, ["risk", "plan", "investigate"]);
}

#[tokio::test]
async fn a_blocked_hypothesis_is_a_gap_and_an_inconclusive_one_is_disclosed() {
    let reply = |outcome: &'static str| {
        scripted(move |r, _| match step(r) {
            "investigate" => text(
                &json!({"outcome": outcome, "reason": "Unknown.", "candidates": []}).to_string(),
            ),
            _ => default_reply(r),
        })
    };
    let blocked = review_fault_crate(reply("blocked"), run_with_budget(base_budget())).await;
    assert_eq!(blocked.review.status, ReviewStatus::Partial);
    assert_eq!(codes(&blocked), ["reduced_context", "truncated"]);

    let unsure = review_fault_crate(reply("inconclusive"), run_with_budget(base_budget())).await;
    assert_eq!(unsure.review.status, ReviewStatus::Complete);
    assert!(
        unsure
            .review
            .summary
            .as_ref()
            .unwrap()
            .contains("1 hypothesis"),
        "{:?}",
        unsure.review.summary
    );
    // Positive control: a settled review says nothing of the kind.
    let settled = review_fault_crate(reply("refuted"), run_with_budget(base_budget())).await;
    assert!(!settled.review.summary.as_ref().unwrap().contains("settled"));
}

#[tokio::test(start_paused = true)]
async fn the_reserved_time_holds_inside_a_running_investigation() {
    // Ten virtual seconds an event: risk and plan take 60 s, a tool turn 20 s, and with a
    // 200 s deadline the cutoff is 150 s, so the fifth turn is the last with tools.
    let provider = Arc::new(Scripted {
        reply: Box::new(|r, _| match step(r) {
            "investigate" if r.tools.is_empty() => text(&refuted()),
            "investigate" => always_new_call(r),
            _ => default_reply(r),
        }),
        requests: Mutex::new(Vec::new()),
        delay: Duration::from_secs(10),
        at_finish: Arc::new(|_| {}),
    });
    let mut run = run_with_budget(base_budget());
    run.deadline = Some(tokio::time::Instant::now() + Duration::from_secs(200));
    let outcome = review_fault_crate(provider, run).await;
    assert_eq!(step_count(&outcome, "tool_actions"), 5);
    assert_eq!(codes(&outcome), ["budget_exhausted", "deadline"]);
    assert_eq!(
        outcome.review.cleared_risks.len(),
        1,
        "it still got its answer"
    );
}

#[tokio::test]
async fn the_reserved_tokens_hold_inside_a_running_investigation() {
    let provider = scripted(|r, _| match step(r) {
        "investigate" if r.tools.is_empty() => text(&refuted()),
        "investigate" => always_new_call(r),
        _ => default_reply(r),
    });
    let limits = BudgetLimits {
        input_tokens: 24_000,
        output_tokens: 40_000,
        cost_usd: None,
    };
    let outcome = review_fault_crate(provider, run_with_budget(limits)).await;
    assert_eq!(codes(&outcome), ["budget_exhausted", "tokens"]);
    assert!(step_count(&outcome, "tool_actions") < 12);
    assert_eq!(
        outcome.review.cleared_risks.len(),
        1,
        "it still got its answer"
    );
}

// ---- The record of the run ----

#[tokio::test]
async fn step_events_are_ordered_and_each_model_call_has_its_event() {
    let provider = scripted(|r, _| default_reply(r));
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    let seqs: Vec<u64> = outcome.events.iter().map(|e| e.seq).collect();
    let want: Vec<u64> = (1..=outcome.events.len() as u64).collect();
    assert_eq!(seqs, want);
    let steps: Vec<String> = outcome
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::StepStarted { step } => Some(step.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        steps,
        [
            "snapshot",
            "dispositions",
            "changed_symbols",
            "risk",
            "plan",
            "investigate"
        ]
    );
    let calls: Vec<(&str, &str, &str)> = outcome
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            RunEventKind::ModelCallCompleted {
                step,
                role,
                binding,
                ..
            } => Some((step.as_deref().unwrap(), role.as_str(), binding.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        [
            ("risk", "risk-classifier", "smart"),
            ("plan", "review", "smart"),
            ("investigate", "review", "smart"),
        ]
    );
    assert_eq!(outcome.records.len(), 3);
    assert_eq!(outcome.review.usage.model_calls, 3);
    assert_eq!(outcome.review.usage.input_tokens, 300);
    // No provider reported a price here: unknown, not zero.
    assert_eq!(outcome.review.usage.cost_usd, None);
}

fn priced(body: &str, cost: Option<f64>) -> Reply {
    vec![
        Ok(Event::TextDelta { text: body.into() }),
        Ok(Event::Usage(Usage {
            input_tokens: Some(100),
            output_tokens: Some(10),
            cost_usd: cost,
        })),
        Ok(Event::Finish {
            reason: FinishReason::Stop,
        }),
    ]
}

#[tokio::test]
async fn one_unpriced_call_makes_the_total_cost_unknown() {
    let provider = scripted(|r, _| {
        let cost = (step(r) != "risk").then_some(0.5);
        let body = match step(r) {
            "risk" => risk_ok(),
            "plan" => plan_of(&[hypothesis("src/lib.rs", "One")]),
            _ => refuted(),
        };
        priced(&body, cost)
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.usage.model_calls, 3);
    assert_eq!(outcome.review.usage.cost_usd, None);
    // Positive control: all three priced adds up.
    let provider = scripted(|r, _| {
        let body = match step(r) {
            "risk" => risk_ok(),
            "plan" => plan_of(&[hypothesis("src/lib.rs", "One")]),
            _ => refuted(),
        };
        priced(&body, Some(0.5))
    });
    let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.usage.cost_usd, Some(1.5));
}

#[tokio::test]
async fn only_a_security_candidate_is_marked_as_one() {
    for (category, want) in [("security", true), ("correctness", false)] {
        let provider = scripted(move |r, _| {
            if step(r) != "investigate" {
                return default_reply(r);
            }
            if !was_answered(r) {
                return tools(&[("c1", "read_file", read_lib())]);
            }
            let ids = evidence_ids(r);
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            let mut found = candidate("src/lib.rs", &ids);
            found["category"] = json!(category);
            text(&supported(&[found]))
        });
        let outcome = review_fault_crate(provider, run_with_budget(base_budget())).await;
        assert_eq!(
            outcome.review.findings[0].security_issue, want,
            "{category}"
        );
    }
}

/// A repository whose only change adds a line inside `one`, which `two` calls.
fn added_line_in_called_function(added: &str) -> TempDir {
    let scripts = TempDir::new("added-line");
    let script = scripts.path().join("make.sh");
    let body = format!(
        r#"#!/usr/bin/env bash
set -euo pipefail
git init -q -b main
mkdir -p src
printf '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n' > Cargo.toml
cat > src/lib.rs <<'GITCKO'
pub fn one() -> u32 {{
    1
}}

pub fn two() -> u32 {{
    one()
}}
GITCKO
git add -A
git commit -q -m Base
git branch base
cat > src/lib.rs <<'GITCKO'
pub fn one() -> u32 {{
    {added}
    1
}}

pub fn two() -> u32 {{
    one()
}}
GITCKO
git add -A
git commit -q -m Head
"#
    );
    std::fs::write(&script, body).unwrap();
    script_repo(&script, "added-line")
}

#[tokio::test]
async fn a_changed_public_function_with_callers_seeds_a_hypothesis() {
    let dir = added_line_in_called_function("let _unused = 1;");
    let provider = scripted(|r, _| match step(r) {
        "plan" => text(&plan_of(&[])),
        _ => default_reply(r),
    });
    let outcome = review_repo(dir.path(), provider, run_with_budget(base_budget())).await;
    let [cleared] = outcome.review.cleared_risks.as_slice() else {
        panic!("{:?}", outcome.review.cleared_risks);
    };
    assert!(cleared.name.contains("one"), "{}", cleared.name);
    assert!(!cleared.name.contains("two"), "{}", cleared.name);
}

#[tokio::test]
async fn without_a_classifier_a_risky_added_line_ranks_high() {
    let dir = added_line_in_called_function("let _p = unsafe { std::ptr::null::<u8>() };");
    let provider = scripted(|r, _| match step(r) {
        "risk" => fail(ErrorKind::Auth),
        _ => default_reply(r),
    });
    let outcome = review_repo(dir.path(), provider, run_with_budget(base_budget())).await;
    let risk = outcome.review.risk_level.unwrap();
    assert_eq!(risk.source, RiskSource::Rules);
    assert_eq!(risk.level, RiskLevel::High);
    // Positive control: a plain change with callers is only medium.
    let dir = added_line_in_called_function("let _unused = 1;");
    let provider = scripted(|r, _| match step(r) {
        "risk" => fail(ErrorKind::Auth),
        _ => default_reply(r),
    });
    let outcome = review_repo(dir.path(), provider, run_with_budget(base_budget())).await;
    assert_eq!(outcome.review.risk_level.unwrap().level, RiskLevel::Medium);
}

#[tokio::test]
async fn the_same_run_gives_the_same_result() {
    let run_once = || async {
        let provider = scripted(|r, _| {
            if step(r) != "investigate" {
                return default_reply(r);
            }
            if !was_answered(r) {
                return tools(&[("c1", "read_file", read_lib())]);
            }
            let ids = evidence_ids(r);
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            text(&supported(&[candidate("src/lib.rs", &ids)]))
        });
        review_fault_crate(provider, run_with_budget(base_budget())).await
    };
    let (one, two) = (run_once().await, run_once().await);
    assert_eq!(
        serde_json::to_value(&one.review).unwrap(),
        serde_json::to_value(&two.review).unwrap()
    );
    assert!(!one.review.findings.is_empty());
}

#[tokio::test]
async fn canary_model_text_and_repo_content_stay_out_of_events_and_reasons() {
    let canary = format!("{}-{}", "canary", "c4f3");
    let echoed = canary.clone();
    let provider = scripted(move |r, _| match step(r) {
        // A refusal-free stream of nonsense naming the canary everywhere it can.
        "risk" => text(&echoed),
        "plan" => text(&plan_of(&[hypothesis("src/lib.rs", &echoed)])),
        _ => fail(ErrorKind::Server),
    });
    let outcome = review_fault_crate(provider.clone(), run_with_budget(base_budget())).await;
    // Positive control: the hypothesis did reach the model, so the canary was in play.
    assert!(all_prompt_text(&provider.requests.lock().unwrap()).contains(&canary));
    let events = serde_json::to_string(&outcome.events).unwrap();
    assert!(!events.contains(&canary));
    let records = serde_json::to_string(&outcome.records).unwrap();
    assert!(!records.contains(&canary));
    assert!(!outcome.review.reason_codes.join(" ").contains(&canary));
    assert!(!outcome.review.summary.clone().unwrap().contains(&canary));
}

#[test]
fn the_budget_is_the_documented_one() {
    let budget = base_budget();
    assert_eq!(budget.input_tokens, 400_000);
    assert_eq!(budget.output_tokens, 40_000);
}
