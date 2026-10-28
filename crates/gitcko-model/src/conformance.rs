// SPDX-License-Identifier: AGPL-3.0-or-later

//! What every adapter must do, as checks its own `tests/conformance.rs` calls: offline
//! against the wire fixtures it serves from a mock server, and with `--live` against
//! the real provider (`cargo xtask conformance`). The fixtures for each check say what
//! the server sends; the check only sees provider-neutral events, so both adapters are
//! held to exactly the same behaviour.
//!
//! Timeouts aren't here: the router owns them and its tests cover them for any
//! provider.

use futures_util::StreamExt;
use gitcko_protocol::config::ModelRole;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{
    ErrorKind, Event, FinishReason, Message, ModelProvider, PromptRef, ProviderError, Request,
    ResponseFormat, Tool, ToolCall, retryable,
};

/// A short plain request. `max_output` stays small so a live run costs cents.
pub fn request() -> Request {
    Request {
        role: ModelRole::Summary,
        prompt: PromptRef {
            id: "conformance".into(),
            version: "1".into(),
        },
        messages: vec![
            Message::System {
                text: "Answer in one word.".into(),
            },
            Message::User {
                text: "Say hello.".into(),
            },
        ],
        tools: vec![],
        response_format: None,
        max_output: 64,
        reasoning_effort: None,
        temperature: None,
    }
}

/// [`request`] asking for `{"ok": true}` under a JSON schema.
pub fn structured_request() -> Request {
    let mut request = request();
    request.messages[1] = Message::User {
        text: "Reply with ok set to true.".into(),
    };
    request.response_format = Some(ResponseFormat {
        name: "ok".into(),
        schema: json!({
            "type": "object",
            "properties": {"ok": {"type": "boolean"}},
            "required": ["ok"],
            "additionalProperties": false,
        }),
    });
    request
}

/// [`request`] offering one tool, `lookup(q)`.
pub fn tool_request() -> Request {
    let mut request = request();
    request.messages[1] = Message::User {
        text: "Look up x.".into(),
    };
    request.tools = vec![Tool {
        name: "lookup".into(),
        description: "Looks a word up.".into(),
        parameters: json!({
            "type": "object",
            "properties": {"q": {"type": "string"}},
            "required": ["q"],
        }),
    }];
    request
}

async fn collect(
    provider: &dyn ModelProvider,
    request: Request,
) -> Vec<Result<Event, ProviderError>> {
    provider
        .stream(request, CancellationToken::new())
        .collect()
        .await
}

fn text_of(events: &[Result<Event, ProviderError>]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            Ok(Event::TextDelta { text }) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Served: "Hello" in more than one delta, then 12 input and 2 output tokens.
pub async fn text(provider: &dyn ModelProvider) {
    let events = collect(provider, request()).await;
    let deltas = events
        .iter()
        .filter(|e| matches!(e, Ok(Event::TextDelta { .. })))
        .count();
    assert!(deltas > 1, "the text streams in pieces: {events:?}");
    assert_eq!(text_of(&events), "Hello");
    let n = events.len();
    assert!(
        matches!(&events[n - 2], Ok(Event::Usage(u)) if u.input_tokens == Some(12) && u.output_tokens == Some(2))
    );
    assert_eq!(
        events[n - 1],
        Ok(Event::Finish {
            reason: FinishReason::Stop
        })
    );
}

/// Served: `{"ok":true}` as text, split across deltas.
pub async fn structured(provider: &dyn ModelProvider) {
    let events = collect(provider, structured_request()).await;
    let value: serde_json::Value =
        serde_json::from_str(&text_of(&events)).expect("structured output is JSON");
    assert_eq!(value, json!({"ok": true}));
    assert_eq!(
        events.last(),
        Some(&Ok(Event::Finish {
            reason: FinishReason::Stop
        }))
    );
}

/// Served: one call to `lookup` with `{"q":"x"}`.
pub async fn tool_call(provider: &dyn ModelProvider) {
    let events = collect(provider, tool_request()).await;
    let calls: Vec<&ToolCall> = events
        .iter()
        .filter_map(|e| match e {
            Ok(Event::ToolCall(call)) => Some(call),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1, "{events:?}");
    assert_eq!(calls[0].name, "lookup");
    assert!(!calls[0].id.is_empty());
    let arguments: serde_json::Value = serde_json::from_str(&calls[0].arguments).unwrap();
    assert_eq!(arguments, json!({"q": "x"}));
    assert_eq!(
        events.last(),
        Some(&Ok(Event::Finish {
            reason: FinishReason::ToolCalls
        }))
    );
}

/// A token canceled before the call starts: `Canceled` and nothing after, with no
/// request needed.
pub async fn canceled_before_start(provider: &dyn ModelProvider) {
    let cancel = CancellationToken::new();
    let mut events = provider.stream(request(), cancel.clone());
    cancel.cancel();
    assert_eq!(
        events.next().await,
        Some(Err(ProviderError::new(ErrorKind::Canceled)))
    );
    assert_eq!(events.next().await, None);
}

/// Served: nothing, ever. Once `arrived` says the stalled server holds the request,
/// canceling ends the call with `Canceled` promptly and nothing after, so an adapter
/// that can't stop an in-flight request fails here.
pub async fn stall_then_cancel(provider: &dyn ModelProvider, arrived: impl Future<Output = ()>) {
    let cancel = CancellationToken::new();
    let mut events = provider.stream(request(), cancel.clone());
    tokio::select! {
        event = events.next() => panic!("a stalled call produced {event:?}"),
        () = arrived => {}
    }
    cancel.cancel();
    let next = tokio::time::timeout(PROMPT, events.next()).await;
    assert_eq!(
        next.expect("canceled promptly"),
        Some(Err(ProviderError::new(ErrorKind::Canceled)))
    );
    assert_eq!(events.next().await, None);
}

/// How long a canceled call may take to say so.
const PROMPT: std::time::Duration = std::time::Duration::from_secs(10);

/// Served: HTTP `status` with an error body. The status is kept, and the kind is
/// retried exactly when the status is 429 or 5xx.
pub async fn status(provider: &dyn ModelProvider, status: u16) {
    let events = collect(provider, request()).await;
    let [Err(error)] = &events[..] else {
        panic!("one error, nothing else: {events:?}");
    };
    assert_eq!(error.status, Some(status));
    let transient = status == 429 || status >= 500;
    assert_eq!(
        retryable(&error.kind),
        transient,
        "{status} → {:?}",
        error.kind
    );
    if status == 401 {
        assert_eq!(error.kind, ErrorKind::Auth);
    }
}

/// Live: a real model answers with some text, reports input tokens and finishes.
pub async fn live_text(provider: &dyn ModelProvider) {
    let events = collect(provider, request()).await;
    assert!(!text_of(&events).trim().is_empty(), "{events:?}");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Ok(Event::Usage(u)) if u.input_tokens.is_some())),
        "{events:?}"
    );
    assert!(
        matches!(events.last(), Some(Ok(Event::Finish { .. }))),
        "{events:?}"
    );
}

/// Live: a real model's structured output finishes cleanly and is exactly the
/// requested object.
pub async fn live_structured(provider: &dyn ModelProvider) {
    let events = collect(provider, structured_request()).await;
    assert_eq!(
        events.last(),
        Some(&Ok(Event::Finish {
            reason: FinishReason::Stop
        })),
        "{events:?}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&text_of(&events)).expect("structured output is JSON");
    let object = value.as_object().expect("an object");
    assert!(object.len() == 1 && object["ok"].is_boolean(), "{value}");
}

/// Live: canceling after the first event ends a real call with `Canceled`, even when
/// the rest of the answer has already arrived.
pub async fn live_cancel_mid_stream(provider: &dyn ModelProvider) {
    let cancel = CancellationToken::new();
    let mut events = provider.stream(request(), cancel.clone());
    let first = events.next().await;
    assert!(matches!(first, Some(Ok(_))), "{first:?}");
    cancel.cancel();
    let next = tokio::time::timeout(PROMPT, events.next()).await;
    assert_eq!(
        next.expect("canceled promptly"),
        Some(Err(ProviderError::new(ErrorKind::Canceled)))
    );
    assert_eq!(events.next().await, None);
}
