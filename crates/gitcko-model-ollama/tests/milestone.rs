// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Phase 3 milestone, end to end: one `[models]` config with an on-device Ollama
//! binding and a remote OpenRouter binding, resolved and routed with both real
//! adapters, each talking to its own local mock server. However the on-device call
//! fails, nothing reaches the remote server; the remote one is reachable, so its
//! zero hits mean something. The routing proptest covers the same rule with fake
//! providers; this checks the real wiring. No network.

use std::collections::BTreeMap;
use std::sync::Arc;

use gitcko_model::{
    BudgetLimits, CallOutcome, DestinationClass, Message, ModelProvider, PromptRef, Request,
    RoleState, Router, Run, RunBudget, resolve,
};
use gitcko_model_ollama::Ollama;
use gitcko_model_openai::OpenRouter;
use gitcko_protocol::config::{ModelRole, UserConfig};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// `fast` (summary) is on device, `smart` (review) is remote, and each class lists
/// the other as a fallback, so only the locality rule keeps `fast` on the device.
const CONFIG: &str = r#"
[models.bindings.fast]
provider = "ollama"
model = "example-coder:7b"

[models.bindings.smart]
provider = "openrouter"
model = "vendor/example-1"

[models.fallbacks]
fast = ["fast", "smart"]
smart = ["smart", "fast"]
"#;

fn fixture(provider: &str, name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../tests/fixtures/providers/{provider}/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

/// The two mock servers and a router over the real adapters pointed at them.
struct Setup {
    ollama: MockServer,
    openrouter: MockServer,
    router: Router,
}

/// `show` and `chat` are what the mock Ollama answers; the mock OpenRouter always
/// answers with text.
async fn setup(show: ResponseTemplate, chat: ResponseTemplate) -> Setup {
    let ollama = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(show)
        .mount(&ollama)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(chat)
        .mount(&ollama)
        .await;
    let openrouter = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_bytes(fixture("openrouter", "text.sse")),
        )
        .mount(&openrouter)
        .await;

    let models = toml::from_str::<UserConfig>(CONFIG)
        .unwrap()
        .models
        .unwrap();
    let port = ollama.address().port();
    let env = BTreeMap::from([
        // Built at run time so no key-shaped literal is committed (MW-13).
        (
            "OPENROUTER_API_KEY".to_string(),
            format!("{}-{}", "test", "not-a-real-key"),
        ),
        ("OLLAMA_HOST".to_string(), format!("127.0.0.1:{port}")),
    ]);
    let models = resolve(&models, &env).unwrap();
    let fast = models.bindings["fast"].clone();
    let smart = models.bindings["smart"].clone();
    assert_eq!(fast.destination.class, DestinationClass::OnDevice);
    assert_eq!(smart.destination.class, DestinationClass::CustomerRemote);

    let providers: BTreeMap<String, Arc<dyn ModelProvider>> = BTreeMap::from([
        (
            "fast".to_string(),
            Arc::new(Ollama::new(fast).unwrap()) as Arc<dyn ModelProvider>,
        ),
        (
            "smart".to_string(),
            Arc::new(OpenRouter::at(smart, &openrouter.uri()).unwrap()) as Arc<dyn ModelProvider>,
        ),
    ]);
    Setup {
        ollama,
        openrouter,
        router: Router::new(models, providers),
    }
}

impl Setup {
    async fn call(&self, run: &mut Run, role: ModelRole) -> CallOutcome {
        let request = Request {
            role,
            prompt: PromptRef {
                id: "milestone".into(),
                version: "1".into(),
            },
            messages: vec![Message::User {
                text: "fn main() {}".into(),
            }],
            tools: vec![],
            response_format: None,
            max_output: 64,
            reasoning_effort: None,
            temperature: None,
        };
        self.router.call(run, request).await
    }

    async fn hits(server: &MockServer, at: &str) -> usize {
        let requests = server.received_requests().await.unwrap();
        requests.iter().filter(|r| r.url.path() == at).count()
    }

    async fn remote_hits(&self) -> usize {
        self.openrouter.received_requests().await.unwrap().len()
    }
}

fn run() -> Run {
    Run::new(
        "run-1",
        RunBudget::new(BudgetLimits {
            input_tokens: 100_000,
            output_tokens: 10_000,
            cost_usd: None,
        }),
    )
}

fn local_show() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({}))
}

fn local_chat() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_bytes(fixture("ollama", "text.ndjson"))
}

fn answered_by(outcome: &CallOutcome) -> &str {
    &outcome.result.as_ref().expect("an answer").binding
}

// Real time, not paused: these calls wait on real sockets, and paused time would
// fire the router's timeouts while they do. Each failure here isn't retried, so no
// test sleeps through a backoff.

#[tokio::test]
async fn a_failing_on_device_call_never_falls_back_to_the_remote_binding() {
    let setup = setup(local_show(), ResponseTemplate::new(400)).await;
    let outcome = setup.call(&mut run(), ModelRole::Summary).await;
    assert_eq!(Setup::hits(&setup.ollama, "/api/chat").await, 1);
    assert_eq!(setup.remote_hits().await, 0);
    assert_eq!(outcome.state(), RoleState::Unavailable);
}

#[tokio::test]
async fn a_cloud_model_behind_local_ollama_stops_without_reaching_either_server() {
    let show = ResponseTemplate::new(200)
        .set_body_json(json!({"remote_host": "https://ollama.example:443"}));
    let setup = setup(show, local_chat()).await;
    let outcome = setup.call(&mut run(), ModelRole::Summary).await;
    assert_eq!(Setup::hits(&setup.ollama, "/api/chat").await, 0);
    assert_eq!(setup.remote_hits().await, 0);
    assert_eq!(outcome.state(), RoleState::Unavailable);
}

#[tokio::test]
async fn a_local_only_run_answers_a_remote_role_on_device() {
    let setup = setup(local_show(), local_chat()).await;
    let mut run = run();
    run.local_only = true;
    let outcome = setup.call(&mut run, ModelRole::Review).await;
    assert_eq!(setup.remote_hits().await, 0);
    assert_eq!(answered_by(&outcome), "fast");
    assert_eq!(outcome.state(), RoleState::FallbackUsed);
}

/// The control: the same config does reach the remote server when locality allows.
#[tokio::test]
async fn the_remote_binding_is_reachable_when_locality_allows() {
    let setup = setup(local_show(), local_chat()).await;
    let outcome = setup.call(&mut run(), ModelRole::Review).await;
    assert_eq!(answered_by(&outcome), "smart");
    assert_eq!(setup.remote_hits().await, 1);
    assert_eq!(Setup::hits(&setup.ollama, "/api/chat").await, 0);
}
