// SPDX-License-Identifier: AGPL-3.0-or-later

//! The shared conformance checks (`gitcko_model::conformance`) for OpenRouter: offline
//! from SSE fixtures, and with `cargo xtask conformance --provider openrouter --live`
//! against the real API (the `live_` tests, ignored otherwise).

use std::collections::BTreeMap;

use gitcko_model::conformance;
use gitcko_model::{Destination, DestinationClass, ResolvedBinding, Secret};
use gitcko_model_openai::OpenRouter;
use gitcko_protocol::config::{
    DataPolicy, ModelFamily, ModelProvider as Provider, ModelRole, UserConfig,
};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../tests/fixtures/providers/openrouter/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

/// A mock OpenRouter answering every call with `response`, and an adapter for it. The
/// server is returned so it lives as long as the test.
async fn serve(response: ResponseTemplate) -> (MockServer, OpenRouter) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(response)
        .mount(&server)
        .await;
    let binding = ResolvedBinding {
        name: "fast".into(),
        provider: Provider::Openrouter,
        model: "vendor/example-1".into(),
        family: ModelFamily::OpenWeight,
        reasoning_effort: None,
        data_policy: Some(DataPolicy::DenyCollection),
        endpoint: None,
        context: None,
        destination: Destination {
            class: DestinationClass::CustomerRemote,
            host: "openrouter.ai".into(),
            port: 443,
        },
        credential: Some(Secret::new(format!("sk-or-v1-{}", "0".repeat(64)))),
    };
    let adapter = OpenRouter::at(binding, &server.uri()).unwrap();
    (server, adapter)
}

fn sse(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_bytes(fixture(name))
}

#[tokio::test]
async fn text() {
    let (_server, adapter) = serve(sse("text.sse")).await;
    conformance::text(&adapter).await;
}

#[tokio::test]
async fn structured() {
    let (_server, adapter) = serve(sse("structured.sse")).await;
    conformance::structured(&adapter).await;
}

#[tokio::test]
async fn tool_call() {
    let (_server, adapter) = serve(sse("tool_call.sse")).await;
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
        let body = format!(r#"{{"error":{{"code":{status},"message":"x"}}}}"#);
        let (_server, adapter) = serve(ResponseTemplate::new(status).set_body_string(body)).await;
        conformance::status(&adapter, status).await;
    }
}

/// The real API, with the key and model from the environment (`cargo xtask
/// conformance` checks they're set before running these).
fn live() -> OpenRouter {
    let model = std::env::var("GITCKO_LIVE_MODEL").expect("GITCKO_LIVE_MODEL");
    let config: UserConfig = toml::from_str(&format!(
        "[models.bindings.fast]\nprovider = \"openrouter\"\nmodel = \"{model}\""
    ))
    .unwrap();
    let env: BTreeMap<String, String> = std::env::vars()
        .filter(|(k, _)| k == "OPENROUTER_API_KEY")
        .collect();
    let models = gitcko_model::resolve(config.models.as_ref().unwrap(), &env).unwrap();
    OpenRouter::new(models.binding_for(ModelRole::Summary).unwrap().clone()).unwrap()
}

#[tokio::test]
#[ignore = "live: cargo xtask conformance --provider openrouter --live"]
async fn live_text() {
    conformance::live_text(&live()).await;
}

#[tokio::test]
#[ignore = "live: cargo xtask conformance --provider openrouter --live"]
async fn live_structured() {
    conformance::live_structured(&live()).await;
}

#[tokio::test]
#[ignore = "live: cargo xtask conformance --provider openrouter --live"]
async fn live_stall_then_cancel() {
    conformance::stall_then_cancel(&live()).await;
}
