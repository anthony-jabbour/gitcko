// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider trait is usable as `dyn`, the fake plays its script, and the call
//! record has the documented shape.

use futures_util::StreamExt;
use gitcko_model::{
    CallRecord, ErrorKind, Event, FakeProvider, Fallback, FinishReason, InversionDecision, Message,
    ModelProvider, PromptRef, ProviderError, Request, RoleState, Usage,
};
use gitcko_protocol::config::{ModelFamily, ModelProvider as Provider, ModelRole, ReasoningEffort};
use tokio_util::sync::CancellationToken;

fn request() -> Request {
    Request {
        messages: vec![Message::User {
            text: "hello".into(),
        }],
        tools: vec![],
        response_format: None,
        max_output: 16,
        reasoning_effort: None,
        temperature: None,
    }
}

#[tokio::test]
async fn the_fake_plays_one_scripted_response_per_call() {
    let fake = FakeProvider::new(vec![
        vec![
            Ok(Event::TextDelta { text: "hi".into() }),
            Ok(Event::Finish {
                reason: FinishReason::Stop,
            }),
        ],
        vec![Err(ProviderError {
            kind: ErrorKind::Server,
            status: Some(503),
        })],
    ]);
    let provider: &dyn ModelProvider = &fake;
    let first: Vec<_> = provider
        .stream(request(), CancellationToken::new())
        .collect()
        .await;
    assert_eq!(first.len(), 2);
    let second: Vec<_> = provider
        .stream(request(), CancellationToken::new())
        .collect()
        .await;
    assert_eq!(second[0].as_ref().unwrap_err().status, Some(503));
    // Past the script: a typed error, not a panic.
    let third: Vec<_> = provider
        .stream(request(), CancellationToken::new())
        .collect()
        .await;
    assert!(matches!(
        third[0],
        Err(ProviderError {
            kind: ErrorKind::InvalidResponse(_),
            ..
        })
    ));
    assert_eq!(fake.calls(), 3);
}

#[test]
fn call_record_shape() {
    let record = CallRecord {
        run_id: "rv_example".into(),
        role: ModelRole::Judge,
        policy: "default@1".into(),
        provider: Provider::Openrouter,
        model: "openai/example-2".into(),
        family: ModelFamily::Openai,
        reasoning_effort: Some(ReasoningEffort::Medium),
        prompt: PromptRef {
            id: "judge".into(),
            version: "1".into(),
        },
        attempt: 2,
        fallback: Some(Fallback {
            from: "smart".into(),
            reason: "primary_failed",
        }),
        inversion: Some(InversionDecision::Applied),
        input_tokens: Some(1200),
        output_tokens: None,
        cost_usd: None,
        first_token_ms: Some(850),
        total_ms: 4100,
        outcome: RoleState::FallbackUsed,
    };
    insta::assert_json_snapshot!(record);
    let _ = Usage::default();
}
