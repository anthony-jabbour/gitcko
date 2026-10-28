// SPDX-License-Identifier: AGPL-3.0-or-later

//! The OpenRouter adapter against hand-written SSE fixtures and a local mock server:
//! decoding, request shaping, the error table and hostile responses. No network.

use std::time::Duration;

use futures_util::StreamExt;
use gitcko_model::stream::Decode;
use gitcko_model::{
    Destination, DestinationClass, ErrorKind, Event, FinishReason, Message, ModelProvider,
    PromptRef, ProviderError, Request, ResolvedBinding, ResponseFormat, Secret, TimeoutKind, Tool,
    ToolCall, Usage,
};
use gitcko_model_openai::{Decoder, OpenRouter, body, status_error};
use gitcko_protocol::config::{
    DataPolicy, ModelFamily, ModelProvider as Provider, ModelRole, ReasoningEffort,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../tests/fixtures/providers/openrouter/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

/// A key shaped like a real one, built at run time (MW-13).
fn key() -> String {
    format!("sk-or-v1-{}", "0".repeat(64))
}

fn binding() -> ResolvedBinding {
    ResolvedBinding {
        name: "smart".into(),
        provider: Provider::Openrouter,
        model: "anthropic/example-1".into(),
        family: ModelFamily::Anthropic,
        reasoning_effort: None,
        data_policy: Some(DataPolicy::DenyCollection),
        endpoint: None,
        context: None,
        destination: Destination {
            class: DestinationClass::CustomerRemote,
            host: "openrouter.ai".into(),
            port: 443,
        },
        credential: Some(Secret::new(key())),
    }
}

fn request() -> Request {
    Request {
        role: ModelRole::Review,
        prompt: PromptRef {
            id: "review".into(),
            version: "1".into(),
        },
        messages: vec![
            Message::System {
                text: "be brief".into(),
            },
            Message::User { text: "hi".into() },
        ],
        tools: vec![],
        response_format: None,
        max_output: 64,
        reasoning_effort: None,
        temperature: Some(0.2),
    }
}

fn decode_all(bytes: &[u8]) -> Vec<Result<Event, ProviderError>> {
    let mut decoder = Decoder::default();
    let mut events = Vec::new();
    let mut result = decoder.push(bytes, &mut events);
    if result.is_ok() && !matches!(events.last(), Some(Event::Finish { .. })) {
        result = decoder.finish(&mut events);
    }
    let mut out: Vec<_> = events.into_iter().map(Ok).collect();
    out.extend(result.err().map(Err));
    out
}

#[test]
fn text_with_comments_then_usage_and_cost_then_done() {
    assert_eq!(
        decode_all(&fixture("text.sse")),
        [
            Ok(Event::TextDelta { text: "Hel".into() }),
            Ok(Event::TextDelta { text: "lo".into() }),
            Ok(Event::Usage(Usage {
                input_tokens: Some(12),
                output_tokens: Some(2),
                cost_usd: Some(0.00042),
            })),
            Ok(Event::Finish {
                reason: FinishReason::Stop
            }),
        ]
    );
}

#[test]
fn tool_call_fragments_join_into_one_call() {
    let events = decode_all(&fixture("tool_call.sse"));
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
fn an_error_event_as_the_only_event_is_a_failure() {
    let events = decode_all(&fixture("error_only.sse"));
    assert_eq!(
        events,
        [Err(ProviderError {
            kind: ErrorKind::Server,
            status: Some(502)
        })]
    );
}

#[test]
fn a_mid_stream_rate_limit_keeps_its_kind() {
    let data = b"data: {\"error\":{\"code\":429,\"message\":\"x\"},\"choices\":[]}\n\n";
    assert_eq!(
        decode_all(data)[0].as_ref().unwrap_err().kind,
        ErrorKind::RateLimited { retry_after: None }
    );
}

#[test]
fn a_stream_cut_off_before_done_is_invalid() {
    let events = decode_all(&fixture("truncated.sse"));
    assert_eq!(events[0], Ok(Event::TextDelta { text: "cut".into() }));
    assert!(matches!(
        events.last().unwrap(),
        Err(ProviderError {
            kind: ErrorKind::InvalidResponse(_),
            ..
        })
    ));
}

#[test]
fn done_without_a_finish_reason_is_invalid() {
    assert!(matches!(
        decode_all(b"data: [DONE]\n\n")[0],
        Err(ProviderError {
            kind: ErrorKind::InvalidResponse(_),
            ..
        })
    ));
}

#[test]
fn malformed_json_is_invalid() {
    assert!(matches!(
        decode_all(b"data: {not json\n\n")[0],
        Err(ProviderError {
            kind: ErrorKind::InvalidResponse(_),
            ..
        })
    ));
}

#[test]
fn endless_tool_arguments_are_too_large() {
    let part = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"name\":\"f\",\"arguments\":\"{}\"}}}}]}}}}]}}\n\n",
        "a".repeat(512 * 1024)
    );
    let mut decoder = Decoder::default();
    let error = (0..32)
        .find_map(|_| decoder.push(part.as_bytes(), &mut Vec::new()).err())
        .expect("over 8 MiB of tool arguments is refused");
    assert_eq!(error.kind, ErrorKind::TooLarge("tool calls"));
}

#[test]
fn shaping_drops_temperature_for_reasoning_and_maps_data_policy() {
    let mut request = request();
    request.tools = vec![Tool {
        name: "lookup".into(),
        description: "find".into(),
        parameters: json!({"type": "object"}),
    }];
    request.response_format = Some(ResponseFormat {
        name: "findings".into(),
        schema: json!({"type": "object"}),
    });
    let plain = body(&binding(), &request);
    assert_eq!(plain["model"], "anthropic/example-1");
    assert_eq!(plain["stream"], true);
    assert_eq!(plain["max_tokens"], 64);
    assert_eq!(plain["temperature"], json!(0.2f32));
    assert!(plain.get("reasoning").is_none());
    assert_eq!(plain["provider"], json!({"data_collection": "deny"}));
    assert_eq!(plain["tools"][0]["function"]["name"], "lookup");
    assert_eq!(plain["response_format"]["json_schema"]["strict"], true);
    assert_eq!(
        plain["messages"][0],
        json!({"role": "system", "content": "be brief"})
    );

    let mut reasoning = binding();
    reasoning.reasoning_effort = Some(ReasoningEffort::High);
    reasoning.data_policy = Some(DataPolicy::Zdr);
    let shaped = body(&reasoning, &request);
    assert!(shaped.get("temperature").is_none());
    assert_eq!(shaped["reasoning"], json!({"effort": "high"}));
    assert_eq!(shaped["provider"], json!({"zdr": true}));

    reasoning.data_policy = Some(DataPolicy::ProviderDefault);
    assert!(body(&reasoning, &request).get("provider").is_none());
}

#[test]
fn tool_turns_round_trip_into_the_wire_shape() {
    let mut request = request();
    request.messages = vec![
        Message::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                name: "lookup".into(),
                arguments: "{}".into(),
            }],
        },
        Message::Tool {
            call_id: "call_1".into(),
            text: "found".into(),
        },
        Message::Assistant {
            text: "done".into(),
            tool_calls: vec![],
        },
    ];
    let messages = &body(&binding(), &request)["messages"];
    assert_eq!(messages[0]["tool_calls"][0]["function"]["name"], "lookup");
    assert_eq!(
        messages[1],
        json!({"role": "tool", "tool_call_id": "call_1", "content": "found"})
    );
    assert!(messages[2].get("tool_calls").is_none());
}

#[test]
fn the_error_table() {
    let after = Some(Duration::from_secs(7));
    let in_flight = br#"{"error":{"code":402,"message":"x","metadata":{"limit_source":"openrouter_in_flight_budget"}}}"#;
    let key_limit = br#"{"error":{"code":403,"message":"x","metadata":{"limit_source":"openrouter_key_limit"}}}"#;
    let cases: [(u16, Option<Duration>, &[u8], ErrorKind); 13] = [
        (400, None, b"", ErrorKind::BadRequest),
        (401, None, b"", ErrorKind::Auth),
        (402, None, b"{}", ErrorKind::InsufficientCredits),
        (
            402,
            after,
            in_flight,
            ErrorKind::RateLimited { retry_after: after },
        ),
        (403, None, b"", ErrorKind::ContentPolicy),
        (403, None, key_limit, ErrorKind::InsufficientCredits),
        (404, None, b"", ErrorKind::BadRequest),
        (408, None, b"", ErrorKind::Timeout(TimeoutKind::Total)),
        (
            429,
            after,
            b"",
            ErrorKind::RateLimited { retry_after: after },
        ),
        (500, None, b"", ErrorKind::Server),
        (502, None, b"", ErrorKind::Server),
        (503, None, b"", ErrorKind::Overloaded),
        (
            503,
            after,
            b"",
            ErrorKind::RateLimited { retry_after: after },
        ),
    ];
    for (status, retry_after, body, kind) in cases {
        let error = status_error(status, retry_after, body);
        assert_eq!(
            error,
            ProviderError {
                kind,
                status: Some(status)
            },
            "{status}"
        );
    }
}

async fn server(response: ResponseTemplate) -> (MockServer, OpenRouter) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .and(header(
            "authorization",
            format!("Bearer {}", key()).as_str(),
        ))
        .respond_with(response)
        .mount(&server)
        .await;
    let adapter = OpenRouter::at(binding(), &format!("{}/api/v1", server.uri())).unwrap();
    (server, adapter)
}

async fn call(adapter: &OpenRouter) -> Vec<Result<Event, ProviderError>> {
    adapter
        .stream(request(), CancellationToken::new())
        .collect()
        .await
}

#[tokio::test]
async fn streams_over_http_with_the_key_in_the_header() {
    let (server, adapter) = server(
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_bytes(fixture("text.sse")),
    )
    .await;
    let events = call(&adapter).await;
    assert_eq!(
        events.last(),
        Some(&Ok(Event::Finish {
            reason: FinishReason::Stop
        }))
    );
    let sent: serde_json::Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert_eq!(sent, body(&binding(), &request()));
}

#[tokio::test]
async fn a_429_carries_retry_after_and_the_status() {
    let (_server, adapter) =
        server(ResponseTemplate::new(429).insert_header("retry-after", "7")).await;
    assert_eq!(
        call(&adapter).await,
        [Err(ProviderError {
            kind: ErrorKind::RateLimited {
                retry_after: Some(Duration::from_secs(7))
            },
            status: Some(429),
        })]
    );
}

#[tokio::test]
async fn a_200_that_is_not_an_event_stream_is_invalid() {
    let (_server, adapter) =
        server(ResponseTemplate::new(200).set_body_json(json!({"choices": []}))).await;
    assert_eq!(
        call(&adapter).await,
        [Err(ProviderError::new(ErrorKind::InvalidResponse(
            "the response is not an event stream"
        )))]
    );
}

#[tokio::test]
async fn a_huge_error_body_is_read_only_up_to_the_cap() {
    let (_server, adapter) =
        server(ResponseTemplate::new(500).set_body_bytes(vec![b'x'; 9 << 20])).await;
    assert_eq!(
        call(&adapter).await,
        [Err(ProviderError {
            kind: ErrorKind::Server,
            status: Some(500)
        })]
    );
}

#[tokio::test]
async fn no_key_is_an_auth_error_before_any_request() {
    let server = MockServer::start().await;
    let mut keyless = binding();
    keyless.credential = None;
    let adapter = OpenRouter::at(keyless, &server.uri()).unwrap();
    assert_eq!(
        call(&adapter).await,
        [Err(ProviderError::new(ErrorKind::Auth))]
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_connection_is_connect() {
    // Bind then drop, so the port is closed.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let adapter = OpenRouter::at(binding(), &format!("http://127.0.0.1:{port}")).unwrap();
    assert_eq!(
        call(&adapter).await,
        [Err(ProviderError::new(ErrorKind::Connect))]
    );
}

#[tokio::test]
async fn cancel_before_the_response_yields_canceled() {
    let (_server, adapter) =
        server(ResponseTemplate::new(200).set_delay(Duration::from_secs(3600))).await;
    let cancel = CancellationToken::new();
    let mut events = adapter.stream(request(), cancel.clone());
    cancel.cancel();
    assert_eq!(
        events.next().await,
        Some(Err(ProviderError::new(ErrorKind::Canceled)))
    );
    assert_eq!(events.next().await, None);
}

#[tokio::test]
async fn canary_errors_never_echo_the_key_or_the_body() {
    for canary in gitcko_redact::canaries() {
        let body = format!(r#"{{"error":{{"code":400,"message":"{canary}"}}}}"#);
        let (_server, adapter) = server(ResponseTemplate::new(400).set_body_string(body)).await;
        for event in call(&adapter).await {
            let error = event.unwrap_err();
            let shown = format!("{error} {error:?}");
            assert!(!shown.contains(canary.as_str()), "{shown}");
            assert!(!shown.contains(&key()), "{shown}");
        }
    }
}

#[test]
fn a_tool_call_without_a_name_is_invalid() {
    let data = b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\"}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n";
    assert!(matches!(
        decode_all(data)[0],
        Err(ProviderError {
            kind: ErrorKind::InvalidResponse(_),
            ..
        })
    ));
}

#[test]
fn finish_reason_error_without_an_error_object_is_a_server_failure() {
    let data = b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"error\"}]}\n\n";
    assert_eq!(
        decode_all(data),
        [Err(ProviderError::new(ErrorKind::Server))]
    );
}

#[test]
fn endless_empty_tool_calls_are_too_large() {
    let mut decoder = Decoder::default();
    let error = (0..=128)
        .find_map(|i| {
            let part = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":{i}}}]}}}}]}}\n\n"
            );
            decoder.push(part.as_bytes(), &mut Vec::new()).err()
        })
        .expect("a 129th tool call is refused");
    assert_eq!(error.kind, ErrorKind::TooLarge("tool calls"));
}

#[test]
fn events_before_a_framing_error_in_the_same_chunk_come_first() {
    let bytes = [
        &b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n"[..],
        &vec![b'a'; (1 << 20) + 2],
    ]
    .concat();
    assert_eq!(
        decode_all(&bytes),
        [
            Ok(Event::TextDelta { text: "Hi".into() }),
            Err(ProviderError::new(ErrorKind::TooLarge("line"))),
        ]
    );
}

#[test]
fn nothing_after_done_is_read() {
    let mut bytes = fixture("text.sse");
    bytes.extend_from_slice(b"\xff\xfe not even utf-8\n");
    assert_eq!(
        decode_all(&bytes).last(),
        Some(&Ok(Event::Finish {
            reason: FinishReason::Stop
        }))
    );
}

#[tokio::test]
async fn cancel_wins_over_events_already_queued() {
    let body = futures_util::stream::iter([Ok::<_, ProviderError>(fixture("text.sse"))]);
    let cancel = CancellationToken::new();
    let mut events = gitcko_model::stream::decode(body, Decoder::default(), cancel.clone());
    assert_eq!(
        events.next().await,
        Some(Ok(Event::TextDelta { text: "Hel".into() }))
    );
    cancel.cancel();
    assert_eq!(
        events.next().await,
        Some(Err(ProviderError::new(ErrorKind::Canceled)))
    );
    assert_eq!(events.next().await, None);
}

#[tokio::test]
async fn a_redirect_is_never_followed() {
    let elsewhere = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&elsewhere)
        .await;
    let (_server, adapter) =
        server(ResponseTemplate::new(307).insert_header("location", elsewhere.uri().as_str()))
            .await;
    assert_eq!(
        call(&adapter).await,
        [Err(ProviderError {
            kind: ErrorKind::BadRequest,
            status: Some(307)
        })]
    );
    assert!(elsewhere.received_requests().await.unwrap().is_empty());
}
