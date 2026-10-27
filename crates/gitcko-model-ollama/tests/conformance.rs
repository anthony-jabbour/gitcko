// SPDX-License-Identifier: AGPL-3.0-or-later

//! The shared conformance checks (`gitcko_model::conformance`) for Ollama: offline
//! from NDJSON fixtures, and with `cargo xtask conformance --provider ollama --live`
//! against a real Ollama server (the `live_` tests, ignored otherwise).

use std::collections::BTreeMap;

use gitcko_config::Origin;
use gitcko_model::conformance;
use gitcko_model::{Destination, DestinationClass, ResolvedBinding};
use gitcko_model_ollama::Ollama;
use gitcko_protocol::config::{ModelFamily, ModelProvider as Provider, ModelRole, UserConfig};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../tests/fixtures/providers/ollama/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

/// A mock Ollama whose `/api/show` reports a local model and whose `/api/chat`
/// answers with `chat`, and an adapter for it.
async fn serve(chat: ResponseTemplate) -> (MockServer, Ollama) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"model_info": {}})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(chat)
        .mount(&server)
        .await;
    let port = server.address().port();
    let binding = ResolvedBinding {
        name: "fast".into(),
        provider: Provider::Ollama,
        model: "example:1b".into(),
        family: ModelFamily::OpenWeight,
        reasoning_effort: None,
        data_policy: None,
        endpoint: Some(Origin {
            https: false,
            host: "127.0.0.1".into(),
            port: Some(port),
        }),
        context: Some(32_768),
        destination: Destination {
            class: DestinationClass::OnDevice,
            host: "127.0.0.1".into(),
            port,
        },
        credential: None,
    };
    (server, Ollama::new(binding).unwrap())
}

fn ndjson(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "application/x-ndjson")
        .set_body_bytes(fixture(name))
}

#[tokio::test]
async fn text() {
    let (_server, adapter) = serve(ndjson("text.ndjson")).await;
    conformance::text(&adapter).await;
}

#[tokio::test]
async fn structured() {
    let (_server, adapter) = serve(ndjson("structured.ndjson")).await;
    conformance::structured(&adapter).await;
}

#[tokio::test]
async fn tool_call() {
    let (_server, adapter) = serve(ndjson("tool_call.ndjson")).await;
    conformance::tool_call(&adapter).await;
}

#[tokio::test]
async fn stall_then_cancel() {
    let stall = ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(3600));
    let (_server, adapter) = serve(stall).await;
    conformance::stall_then_cancel(&adapter).await;
}

#[tokio::test]
async fn status() {
    for status in [400, 401, 429, 500, 503] {
        let body = json!({"error": "x"});
        let (_server, adapter) = serve(ResponseTemplate::new(status).set_body_json(body)).await;
        conformance::status(&adapter, status).await;
    }
}

/// A real server at `OLLAMA_HOST` (or Ollama's default), with the model from the
/// environment.
fn live() -> Ollama {
    let model = std::env::var("GITCKO_LIVE_MODEL").expect("GITCKO_LIVE_MODEL");
    let config: UserConfig = toml::from_str(&format!(
        "[models.bindings.fast]\nprovider = \"ollama\"\nmodel = \"{model}\""
    ))
    .unwrap();
    let env: BTreeMap<String, String> = std::env::vars()
        .filter(|(k, _)| k == "OLLAMA_HOST")
        .collect();
    let models = gitcko_model::resolve(config.models.as_ref().unwrap(), &env).unwrap();
    Ollama::new(models.binding_for(ModelRole::Summary).unwrap().clone()).unwrap()
}

#[tokio::test]
#[ignore = "live: cargo xtask conformance --provider ollama --live"]
async fn live_text() {
    conformance::live_text(&live()).await;
}

#[tokio::test]
#[ignore = "live: cargo xtask conformance --provider ollama --live"]
async fn live_structured() {
    conformance::live_structured(&live()).await;
}

#[tokio::test]
#[ignore = "live: cargo xtask conformance --provider ollama --live"]
async fn live_stall_then_cancel() {
    conformance::stall_then_cancel(&live()).await;
}
