// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Ollama adapter against hand-written NDJSON fixtures and a local mock server:
//! the locality preflight, decoding, request shaping and errors. No network.

use futures_util::StreamExt;
use gitcko_config::Origin;
use gitcko_model::stream::Decode;
use gitcko_model::{
    Destination, DestinationClass, ErrorKind, Event, FinishReason, Message, ModelProvider,
    PromptRef, ProviderError, Request, ResolvedBinding, ResponseFormat, ToolCall, Usage,
};
use gitcko_model_ollama::{Decoder, Ollama, body, status_error};
use gitcko_protocol::config::{ModelFamily, ModelProvider as Provider, ModelRole, ReasoningEffort};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../tests/fixtures/providers/ollama/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

fn binding(host: &str, port: u16, class: DestinationClass) -> ResolvedBinding {
    ResolvedBinding {
        name: "fast".into(),
        provider: Provider::Ollama,
        model: "qwen3:8b".into(),
        family: ModelFamily::OpenWeight,
        reasoning_effort: None,
        data_policy: None,
        endpoint: Some(Origin {
            https: false,
            host: host.into(),
            port: Some(port),
        }),
        context: Some(32_768),
        destination: Destination {
            class,
            host: host.into(),
            port,
        },
        credential: None,
    }
}

fn request() -> Request {
    Request {
        role: ModelRole::Summary,
        prompt: PromptRef {
            id: "summary".into(),
            version: "1".into(),
        },
        messages: vec![Message::User { text: "hi".into() }],
        tools: vec![],
        response_format: None,
        max_output: 64,
        reasoning_effort: None,
        temperature: None,
    }
}

fn decode_all(bytes: &[u8], on_device: bool) -> Vec<Result<Event, ProviderError>> {
    let mut decoder = Decoder::new(on_device);
    let mut out = Vec::new();
    match decoder.push(bytes) {
        Ok(events) => out.extend(events.into_iter().map(Ok)),
        Err(e) => return vec![Err(e)],
    }
    if !matches!(out.last(), Some(Ok(Event::Finish { .. }))) {
        match decoder.finish() {
            Ok(events) => out.extend(events.into_iter().map(Ok)),
            Err(e) => out.push(Err(e)),
        }
    }
    out
}

#[test]
fn text_then_counts_on_done() {
    assert_eq!(
        decode_all(&fixture("text.ndjson"), true),
        [
            Ok(Event::TextDelta { text: "Hel".into() }),
            Ok(Event::TextDelta { text: "lo".into() }),
            Ok(Event::Usage(Usage {
                input_tokens: Some(12),
                output_tokens: Some(2),
                cost_usd: None,
            })),
            Ok(Event::Finish {
                reason: FinishReason::Stop
            }),
        ]
    );
}

#[test]
fn a_whole_tool_call_with_object_arguments_and_crlf_without_a_final_newline() {
    let events = decode_all(&fixture("tool_call.ndjson"), true);
    assert_eq!(
        events[0],
        Ok(Event::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "lookup".into(),
            arguments: "{\"q\":\"x\"}".into(),
        }))
    );
    assert_eq!(
        events[2],
        Ok(Event::Finish {
            reason: FinishReason::ToolCalls
        })
    );
}

#[test]
fn done_reason_length_is_length() {
    let line = b"{\"message\":{\"content\":\"a\"},\"done\":true,\"done_reason\":\"length\"}\n";
    assert_eq!(
        decode_all(line, true).last(),
        Some(&Ok(Event::Finish {
            reason: FinishReason::Length
        }))
    );
}

#[test]
fn a_chunk_from_a_remote_host_aborts_an_on_device_call() {
    assert_eq!(
        decode_all(&fixture("remote_host.ndjson"), true),
        [Err(ProviderError::new(ErrorKind::LocalityViolation))]
    );
    // A binding already counted as remote may use a cloud model.
    let remote = decode_all(&fixture("remote_host.ndjson"), false);
    assert_eq!(remote[0], Ok(Event::TextDelta { text: "hi".into() }));
}

#[test]
fn a_stream_without_done_is_invalid_and_an_error_line_is_a_failure() {
    assert!(matches!(
        decode_all(&fixture("truncated.ndjson"), true).last(),
        Some(Err(ProviderError {
            kind: ErrorKind::InvalidResponse(_),
            ..
        }))
    ));
    assert_eq!(
        decode_all(&fixture("error.ndjson"), true),
        [Err(ProviderError::new(ErrorKind::Server))]
    );
    assert!(matches!(
        decode_all(b"{not json\n", true)[0],
        Err(ProviderError {
            kind: ErrorKind::InvalidResponse(_),
            ..
        })
    ));
}

#[test]
fn shaping_sets_num_ctx_num_predict_format_and_think() {
    let mut request = request();
    request.response_format = Some(ResponseFormat {
        name: "findings".into(),
        schema: json!({"type": "object"}),
    });
    request.temperature = Some(0.5);
    request.messages = vec![
        Message::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "lookup".into(),
                arguments: "{\"q\":1}".into(),
            }],
        },
        Message::Tool {
            call_id: "call_1".into(),
            text: "found".into(),
        },
    ];
    let mut thinking = binding("127.0.0.1", 11434, DestinationClass::OnDevice);
    thinking.reasoning_effort = Some(ReasoningEffort::Low);
    let shaped = body(&thinking, &request);
    assert_eq!(
        shaped["options"],
        json!({"num_ctx": 32768, "num_predict": 64, "temperature": 0.5})
    );
    assert_eq!(shaped["format"], json!({"type": "object"}));
    assert_eq!(shaped["think"], "low");
    assert_eq!(shaped["stream"], true);
    assert_eq!(
        shaped["messages"][0]["tool_calls"][0]["function"]["arguments"],
        json!({"q": 1})
    );
    assert_eq!(
        shaped["messages"][1],
        json!({"role": "tool", "content": "found", "tool_name": "lookup"})
    );
    let plain = body(
        &binding("127.0.0.1", 11434, DestinationClass::OnDevice),
        &self::request(),
    );
    assert!(plain.get("think").is_none() && plain.get("format").is_none());
}

#[test]
fn the_error_table() {
    use ErrorKind::*;
    for (status, kind) in [
        (400, BadRequest),
        (401, Auth),
        (403, Auth),
        (404, BadRequest),
        (429, RateLimited { retry_after: None }),
        (500, Server),
        (503, Overloaded),
    ] {
        assert_eq!(status_error(status).kind, kind, "{status}");
        assert_eq!(status_error(status).status, Some(status));
    }
}

async fn server(show: ResponseTemplate, chat: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(show)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(chat)
        .mount(&server)
        .await;
    server
}

fn chats(requests: &[wiremock::Request]) -> usize {
    requests
        .iter()
        .filter(|r| r.url.path() == "/api/chat")
        .count()
}

async fn call(adapter: &Ollama) -> Vec<Result<Event, ProviderError>> {
    adapter
        .stream(request(), CancellationToken::new())
        .collect()
        .await
}

#[tokio::test]
async fn a_remote_host_in_show_stops_before_any_chat() {
    let server = server(
        ResponseTemplate::new(200).set_body_json(json!({"remote_host": "https://ollama.com:443"})),
        ResponseTemplate::new(200).set_body_bytes(fixture("text.ndjson")),
    )
    .await;
    let port = server.address().port();
    let adapter = Ollama::new(binding("127.0.0.1", port, DestinationClass::OnDevice)).unwrap();
    assert_eq!(
        call(&adapter).await,
        [Err(ProviderError::new(ErrorKind::LocalityViolation))]
    );
    assert_eq!(chats(&server.received_requests().await.unwrap()), 0);
}

#[tokio::test]
async fn localhost_is_pinned_to_loopback_and_streams() {
    let server = server(
        ResponseTemplate::new(200).set_body_json(json!({"model_info": {}})),
        ResponseTemplate::new(200).set_body_bytes(fixture("text.ndjson")),
    )
    .await;
    let port = server.address().port();
    let adapter = Ollama::new(binding("localhost", port, DestinationClass::OnDevice)).unwrap();
    let events = call(&adapter).await;
    assert_eq!(
        events.last(),
        Some(&Ok(Event::Finish {
            reason: FinishReason::Stop
        }))
    );
    let requests = server.received_requests().await.unwrap();
    let sent: serde_json::Value = requests[1].body_json().unwrap();
    assert_eq!(
        sent,
        body(
            &binding("localhost", port, DestinationClass::OnDevice),
            &request()
        )
    );
}

#[tokio::test]
async fn a_model_that_is_not_pulled_is_a_bad_request_without_a_chat() {
    let server = server(
        ResponseTemplate::new(404).set_body_json(json!({"error": "model not found"})),
        ResponseTemplate::new(200),
    )
    .await;
    let port = server.address().port();
    let adapter = Ollama::new(binding("127.0.0.1", port, DestinationClass::OnDevice)).unwrap();
    assert_eq!(call(&adapter).await, [Err(status_error(404))]);
    assert_eq!(chats(&server.received_requests().await.unwrap()), 0);
}

#[tokio::test]
async fn a_chat_error_status_maps_without_its_text() {
    for canary in gitcko_redact::canaries() {
        let server = server(
            ResponseTemplate::new(200).set_body_json(json!({})),
            ResponseTemplate::new(503).set_body_json(json!({"error": canary})),
        )
        .await;
        let port = server.address().port();
        let adapter = Ollama::new(binding("127.0.0.1", port, DestinationClass::OnDevice)).unwrap();
        let events = call(&adapter).await;
        assert_eq!(events, [Err(status_error(503))]);
        let shown = format!("{:?} {}", events, events[0].as_ref().unwrap_err());
        assert!(!shown.contains(canary.as_str()), "{shown}");
    }
}

#[tokio::test]
async fn cancel_before_the_response_yields_canceled() {
    let server = server(
        ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(3600)),
        ResponseTemplate::new(200),
    )
    .await;
    let port = server.address().port();
    let adapter = Ollama::new(binding("127.0.0.1", port, DestinationClass::OnDevice)).unwrap();
    let cancel = CancellationToken::new();
    let mut events = adapter.stream(request(), cancel.clone());
    cancel.cancel();
    assert_eq!(
        events.next().await,
        Some(Err(ProviderError::new(ErrorKind::Canceled)))
    );
    assert_eq!(events.next().await, None);
}

#[test]
fn tool_arguments_that_are_not_json_go_as_a_string() {
    let mut request = request();
    request.messages = vec![Message::Assistant {
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: "c".into(),
            name: "f".into(),
            arguments: "not json".into(),
        }],
    }];
    let shaped = body(
        &binding("127.0.0.1", 11434, DestinationClass::OnDevice),
        &request,
    );
    assert_eq!(
        shaped["messages"][0]["tool_calls"][0]["function"]["arguments"],
        "not json"
    );
}
