// SPDX-License-Identifier: AGPL-3.0-or-later

//! Record then replay is exact; the key covers what changes an answer and nothing
//! else; a missing cassette fails loudly; nothing secret or local reaches a file.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::StreamExt;
use gitcko_cassette::{Cassette, RecordRefused};
use gitcko_config::Origin;
use gitcko_model::{
    Destination, DestinationClass, ErrorKind, Event, FakeProvider, FinishReason, Message,
    ModelProvider, PromptRef, ProviderError, Request, ResolvedBinding, Secret, ToolCall, Usage,
};
use gitcko_protocol::config::{ModelFamily, ModelProvider as Provider, ModelRole};
use proptest::prelude::*;
use tokio_util::sync::CancellationToken;

fn binding(port: u16, credential: Option<&str>) -> ResolvedBinding {
    ResolvedBinding {
        name: "smart".into(),
        provider: Provider::Ollama,
        model: "fixture-model:7b".into(),
        family: ModelFamily::OpenWeight,
        reasoning_effort: None,
        data_policy: None,
        endpoint: Origin::parse(&format!("http://127.0.0.1:{port}")),
        context: Some(131_072),
        destination: Destination {
            class: DestinationClass::OnDevice,
            host: "127.0.0.1".into(),
            port,
        },
        credential: credential.map(|c| Secret::new(c.to_string())),
    }
}

fn request(text: &str) -> Request {
    Request {
        role: ModelRole::Review,
        prompt: PromptRef {
            id: "review".into(),
            version: "1".into(),
        },
        messages: vec![
            Message::System {
                text: "You review fixture code.".into(),
            },
            Message::User { text: text.into() },
        ],
        tools: vec![],
        response_format: None,
        max_output: 256,
        reasoning_effort: None,
        temperature: Some(0.0),
    }
}

fn response(text: &str) -> Vec<Result<Event, ProviderError>> {
    vec![
        Ok(Event::TextDelta { text: text.into() }),
        Ok(Event::ToolCall(ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"src/lib.rs"}"#.into(),
        })),
        Ok(Event::Usage(Usage {
            input_tokens: Some(12),
            output_tokens: Some(5),
            cost_usd: Some(0.000_125),
        })),
        Ok(Event::Finish {
            reason: FinishReason::Stop,
        }),
    ]
}

fn no_ci() -> BTreeMap<String, String> {
    BTreeMap::new()
}

async fn run(provider: &dyn ModelProvider, request: Request) -> Vec<Result<Event, ProviderError>> {
    provider
        .stream(request, CancellationToken::new())
        .collect()
        .await
}

fn only_file(dir: &std::path::Path) -> Vec<u8> {
    let files: Vec<_> = std::fs::read_dir(dir).unwrap().collect();
    assert_eq!(files.len(), 1, "one cassette per call");
    std::fs::read(files[0].as_ref().unwrap().path()).unwrap()
}

#[tokio::test]
async fn record_then_replay_is_exact_for_events_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let inner = Arc::new(FakeProvider::new(vec![
        response("looks fine"),
        response("looks fine"),
    ]));
    let recorder = Cassette::record(dir.path(), &binding(11434, None), inner, &no_ci()).unwrap();
    let recorded = run(&recorder, request("fn main() {}")).await;
    assert_eq!(
        recorded,
        response("looks fine"),
        "recording passes events through"
    );
    let first = only_file(dir.path());

    let replayed = run(
        &Cassette::replay(dir.path(), &binding(11434, None)),
        request("fn main() {}"),
    )
    .await;
    assert_eq!(replayed, response("looks fine"));

    // Recording the same call again writes the same bytes.
    let _ = run(&recorder, request("fn main() {}")).await;
    assert_eq!(only_file(dir.path()), first);
    assert!(first.ends_with(b"}\n"));
}

#[tokio::test]
async fn a_failed_call_is_passed_through_and_not_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let inner = Arc::new(FakeProvider::new(vec![vec![Err(ProviderError {
        kind: ErrorKind::Server,
        status: Some(503),
    })]]));
    let recorder = Cassette::record(dir.path(), &binding(11434, None), inner, &no_ci()).unwrap();
    let events = run(&recorder, request("x")).await;
    assert_eq!(events[0].as_ref().unwrap_err().status, Some(503));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn a_missing_cassette_fails_with_the_key_and_the_record_command() {
    let dir = tempfile::tempdir().unwrap();
    let cassette = Cassette::replay(dir.path(), &binding(11434, None));
    let key = cassette.key(&request("x"));
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cassette.stream(request("x"), CancellationToken::new())
    }))
    .map(drop)
    .expect_err("a missing cassette must fail the test, never call anything");
    let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(message.contains(&key), "{message}");
    assert!(
        message.contains("cargo xtask cassettes record"),
        "{message}"
    );
    assert!(message.contains("ollama:fixture-model:7b"), "{message}");
}

#[test]
fn recording_is_refused_in_ci() {
    let dir = tempfile::tempdir().unwrap();
    let env = BTreeMap::from([("CI".to_string(), "true".to_string())]);
    let inner = Arc::new(FakeProvider::new(vec![]));
    let refused = Cassette::record(dir.path(), &binding(11434, None), inner, &env);
    assert!(matches!(refused, Err(RecordRefused::InCi)));
}

/// What a recording may never hold. Each one is refused, the file isn't written and the
/// call ends with a typed error instead of `Finish`.
#[tokio::test]
async fn a_recording_with_a_secret_or_a_local_path_is_not_written() {
    let key = format!("sk-or-v1-{}", "c".repeat(48));
    let home = std::env::temp_dir().join("gitcko-home-fixture");
    let cwd = std::env::current_dir().unwrap();
    let mut leaks = vec![
        format!("the key is {key}"),
        "Authorization: Basic Zm9v".to_string(),
        format!("bearer {}", "x".repeat(8)),
        format!("see {}/notes.txt", home.display()),
        format!("see {}/src/lib.rs", cwd.display()),
        format!("see {}", std::env::temp_dir().join("scratch.txt").display()),
    ];
    leaks.extend(gitcko_redact::canaries());
    let env = BTreeMap::from([("HOME".to_string(), home.display().to_string())]);
    for leak in leaks {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(FakeProvider::new(vec![response(&leak)]));
        let recorder =
            Cassette::record(dir.path(), &binding(11434, Some(&key)), inner, &env).unwrap();
        let events = run(&recorder, request("x")).await;
        assert!(
            matches!(
                events.last(),
                Some(Err(ProviderError {
                    kind: ErrorKind::InvalidResponse(_),
                    ..
                }))
            ),
            "{leak}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "{leak}");
    }
}

#[test]
fn canary_a_refusal_never_shows_a_value() {
    let shown = format!("{} {:?}", RecordRefused::InCi, RecordRefused::InCi);
    for canary in gitcko_redact::canaries() {
        assert!(!shown.contains(canary.as_str()));
    }
}

#[tokio::test]
async fn replay_honours_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let inner = Arc::new(FakeProvider::new(vec![response("ok")]));
    let recorder = Cassette::record(dir.path(), &binding(11434, None), inner, &no_ci()).unwrap();
    let _ = run(&recorder, request("x")).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let events: Vec<_> = Cassette::replay(dir.path(), &binding(11434, None))
        .stream(request("x"), cancel)
        .collect()
        .await;
    assert_eq!(events, vec![Err(ProviderError::new(ErrorKind::Canceled))]);
}

fn key(binding: &ResolvedBinding, request: &Request) -> String {
    Cassette::replay("unused", binding).key(request)
}

proptest! {
    /// Anything that can change the answer changes the key.
    #[test]
    fn what_shapes_the_answer_changes_the_key(text in ".{0,40}", other in ".{0,40}") {
        prop_assume!(text != other);
        let base = key(&binding(11434, None), &request(&text));
        prop_assert_ne!(&base, &key(&binding(11434, None), &request(&other)));

        let mut role = request(&text);
        role.role = ModelRole::Judge;
        prop_assert_ne!(&base, &key(&binding(11434, None), &role));

        let mut prompt = request(&text);
        prompt.prompt.version = "2".into();
        prop_assert_ne!(&base, &key(&binding(11434, None), &prompt));

        let mut max = request(&text);
        max.max_output += 1;
        prop_assert_ne!(&base, &key(&binding(11434, None), &max));

        let mut model = binding(11434, None);
        model.model = "fixture-model:8b".into();
        prop_assert_ne!(&base, &key(&model, &request(&text)));

        let mut context = binding(11434, None);
        context.context = Some(32_768);
        prop_assert_ne!(&base, &key(&context, &request(&text)));
    }

    /// Where the call goes, how it authenticates and what the binding is called don't.
    #[test]
    fn where_and_how_a_call_goes_does_not_change_the_key(port in 1u16.., name in "[a-z]{1,12}") {
        let base = key(&binding(11434, None), &request("x"));
        let mut moved = binding(port, Some("a-key-the-cassette-must-ignore"));
        moved.name = name;
        prop_assert_eq!(base, key(&moved, &request("x")));
    }
}

/// The scan reads what the file means, not its JSON escaping: a key split across
/// deltas, or a Windows path whose backslashes JSON doubles, is still caught.
#[tokio::test]
async fn a_leak_split_across_deltas_or_hidden_by_escaping_is_not_written() {
    let key = format!("sk-or-v1-{}", "d".repeat(48));
    let home = r"C:\Users\fixture-user".to_string();
    let env = BTreeMap::from([("USERPROFILE".to_string(), home.clone())]);
    let (head, tail) = key.split_at(12);
    let split = vec![
        Ok(Event::TextDelta { text: head.into() }),
        Ok(Event::TextDelta { text: tail.into() }),
        Ok(Event::Finish {
            reason: FinishReason::Stop,
        }),
    ];
    let mut windows_path = response("");
    windows_path[0] = Ok(Event::TextDelta {
        text: format!(r"see {home}\notes.txt"),
    });
    // A path inside tool-call arguments is escaped twice: once by the arguments' JSON,
    // once by the cassette's.
    let mut in_arguments = response("");
    in_arguments[1] = Ok(Event::ToolCall(ToolCall {
        id: "call_1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({ "path": format!(r"{home}\notes.txt") }).to_string(),
    }));
    // The temp folder by its canonical name (`/private/var/...` on macOS).
    let canonical = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let mut canonical_temp = response("");
    canonical_temp[0] = Ok(Event::TextDelta {
        text: format!("see {}", canonical.join("scratch.txt").display()),
    });
    for script in [split, windows_path, in_arguments, canonical_temp] {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(FakeProvider::new(vec![script]));
        let recorder =
            Cassette::record(dir.path(), &binding(11434, Some(&key)), inner, &env).unwrap();
        let events = run(&recorder, request("x")).await;
        assert!(events.last().unwrap().is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

/// A secret hidden in a JSON property name of tool arguments (escaped so the file's
/// text doesn't show it) is still caught: keys are scanned like values.
#[tokio::test]
async fn a_secret_hidden_in_an_argument_property_name_is_not_written() {
    // A key with no credential shape, so only the registered value can catch it,
    // and only once the property name is decoded.
    let key = format!("fixture{}", "q".repeat(24));
    let escaped = format!(r#"{{"\u0066{}":"x"}}"#, &key[1..]);
    let mut script = response("");
    script[1] = Ok(Event::ToolCall(ToolCall {
        id: "call_1".into(),
        name: "read_file".into(),
        arguments: escaped,
    }));
    let dir = tempfile::tempdir().unwrap();
    let inner = Arc::new(FakeProvider::new(vec![script]));
    let recorder =
        Cassette::record(dir.path(), &binding(11434, Some(&key)), inner, &no_ci()).unwrap();
    let events = run(&recorder, request("x")).await;
    assert!(events.last().unwrap().is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
