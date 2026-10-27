// SPDX-License-Identifier: AGPL-3.0-or-later

//! A proxy from the environment would carry an on-device call off the machine, so the
//! adapter never uses one. Setting `HTTP_PROXY` in this process needs `unsafe`
//! (edition 2024), so the test reruns its own binary with the variable set.

use futures_util::StreamExt;
use gitcko_config::Origin;
use gitcko_model::{
    Destination, DestinationClass, Event, FinishReason, Message, ModelProvider, PromptRef, Request,
    ResolvedBinding,
};
use gitcko_model_ollama::Ollama;
use gitcko_protocol::config::{ModelFamily, ModelProvider as Provider, ModelRole};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

const NDJSON: &str = "{\"message\":{\"content\":\"a\"},\"done\":true}\n";

#[tokio::test]
async fn an_environment_proxy_is_never_used() {
    let ollama = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string(NDJSON))
        .mount(&ollama)
        .await;
    let proxy = MockServer::start().await;
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_calls_ollama", "--ignored", "--nocapture"])
        .env("HTTP_PROXY", proxy.uri())
        .env("http_proxy", proxy.uri())
        .env("ALL_PROXY", proxy.uri())
        .env("PROXY_TEST_PORT", ollama.address().port().to_string())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(proxy.received_requests().await.unwrap().is_empty());
    assert_eq!(ollama.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
#[ignore = "run by an_environment_proxy_is_never_used with a proxy set"]
async fn child_calls_ollama() {
    let port: u16 = std::env::var("PROXY_TEST_PORT").unwrap().parse().unwrap();
    let binding = ResolvedBinding {
        name: "fast".into(),
        provider: Provider::Ollama,
        model: "m:1".into(),
        family: ModelFamily::OpenWeight,
        reasoning_effort: None,
        data_policy: None,
        endpoint: Some(Origin {
            https: false,
            host: "127.0.0.1".into(),
            port: Some(port),
        }),
        context: None,
        destination: Destination {
            class: DestinationClass::OnDevice,
            host: "127.0.0.1".into(),
            port,
        },
        credential: None,
    };
    let request = Request {
        role: ModelRole::Summary,
        prompt: PromptRef {
            id: "p".into(),
            version: "1".into(),
        },
        messages: vec![Message::User { text: "hi".into() }],
        tools: vec![],
        response_format: None,
        max_output: 8,
        reasoning_effort: None,
        temperature: None,
    };
    let events: Vec<_> = Ollama::new(binding)
        .unwrap()
        .stream(request, CancellationToken::new())
        .collect()
        .await;
    assert_eq!(
        events.last(),
        Some(&Ok(Event::Finish {
            reason: FinishReason::Stop
        }))
    );
}
