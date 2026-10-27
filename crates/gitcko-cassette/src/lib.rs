// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recorded model calls (tests/cassettes/README.md). A [`Cassette`] sits where an
//! adapter would, at the [`ModelProvider`] boundary, so the router, budgets and
//! timeouts run exactly as in production while the answer comes from a file.
//!
//! Replay is the default and the only mode CI allows. Recording wraps a real provider,
//! is refused when `CI` is set, and refuses to write a file holding the binding's key,
//! an auth header, a credential-shaped string or a local absolute path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt;
use gitcko_model::{
    ErrorKind, Event, EventStream, ModelProvider, PromptRef, ProviderError, Request,
    ResolvedBinding,
};
use gitcko_protocol::config::{DataPolicy, ModelProvider as Provider, ModelRole, ReasoningEffort};
use gitcko_redact::Redactor;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

/// The cassette format version, part of every key.
const VERSION: u32 = 1;

/// One binding's recorded calls in a directory, one file per key (`<key>.json`).
pub struct Cassette {
    dir: PathBuf,
    binding: BindingKey,
    recorder: Option<Recorder>,
}

/// Why record mode can't start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordRefused {
    /// CI only replays, so a missing cassette fails instead of calling a model.
    #[error(
        "recording model calls is refused when CI is set; record locally and commit the cassette"
    )]
    InCi,
}

struct Recorder {
    inner: Arc<dyn ModelProvider>,
    /// The binding's key, the local paths and credential shapes a file must not hold.
    redactor: Redactor,
}

/// The part of a binding that can change the answer. The endpoint, port, credential
/// and binding name are left out: moving a server or rotating a key replays the same.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct BindingKey {
    provider: Provider,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
    data_policy: Option<DataPolicy>,
    context: Option<u32>,
}

/// What a cassette file holds. Field order is fixed, so the same call writes the same
/// bytes.
#[derive(Serialize, Deserialize)]
struct File {
    v: u32,
    key: String,
    role: ModelRole,
    binding: BindingKey,
    prompt: PromptRef,
    events: Vec<Event>,
}

impl Cassette {
    /// Replays the calls recorded in `dir` for `binding`.
    pub fn replay(dir: impl Into<PathBuf>, binding: &ResolvedBinding) -> Cassette {
        Cassette {
            dir: dir.into(),
            binding: binding_key(binding),
            recorder: None,
        }
    }

    /// Sends each call to `inner` and records its response in `dir`, overwriting the
    /// file for the same key. `env` is the process environment: `CI` refuses recording,
    /// and `HOME`/`USERPROFILE` is a path no file may contain.
    pub fn record(
        dir: impl Into<PathBuf>,
        binding: &ResolvedBinding,
        inner: Arc<dyn ModelProvider>,
        env: &BTreeMap<String, String>,
    ) -> Result<Cassette, RecordRefused> {
        if env.get("CI").is_some_and(|v| !v.is_empty()) {
            return Err(RecordRefused::InCi);
        }
        let mut redactor = Redactor::new();
        if let Some(credential) = &binding.credential {
            redactor = redactor.secret(credential.expose());
        }
        let home = ["HOME", "USERPROFILE"].iter().filter_map(|v| env.get(*v));
        let cwd = std::env::current_dir().ok();
        let paths = home
            .map(PathBuf::from)
            .chain([std::env::temp_dir()])
            .chain(cwd);
        // ponytail: matched case-sensitively; macOS and Windows paths aren't, so add
        // case folding if a recording ever spells one differently. A canonical name
        // (macOS's `/private/var/...`) contains the plain one, so it is caught too.
        for path in paths {
            redactor = redactor.path(&path, "local");
        }
        Ok(Cassette {
            dir: dir.into(),
            binding: binding_key(binding),
            recorder: Some(Recorder { inner, redactor }),
        })
    }

    /// The SHA-256 of the call's canonical JSON: format version, binding, role, prompt
    /// and the rendered request. Run ids, attempts, timeouts and clocks are not in a
    /// [`Request`], so they can't reach the key.
    pub fn key(&self, request: &Request) -> String {
        let input = serde_json::json!({
            "v": VERSION,
            "binding": self.binding,
            "role": request.role,
            "prompt": request.prompt,
            "request": {
                "messages": request.messages,
                "tools": request.tools,
                "response_format": request.response_format,
                "max_output": request.max_output,
                "reasoning_effort": request.reasoning_effort,
                "temperature": request.temperature,
            },
        });
        let canonical = serde_json::to_vec(&sorted(input)).expect("JSON values serialize");
        Sha256::digest(canonical)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }
}

impl ModelProvider for Cassette {
    /// # Panics
    ///
    /// In replay mode, when no cassette exists for the call. Cassettes are test data,
    /// and a missing one is a test that must be recorded, never a reason to call a
    /// model; the message names the key and the command.
    fn stream(&self, request: Request, cancel: CancellationToken) -> EventStream {
        let key = self.key(&request);
        let path = self.path(&key);
        match &self.recorder {
            None => replay(&path, &key, &self.binding, cancel),
            Some(recorder) => {
                let file = File {
                    v: VERSION,
                    key,
                    role: request.role,
                    binding: self.binding.clone(),
                    prompt: request.prompt.clone(),
                    events: Vec::new(),
                };
                let inner = recorder.inner.stream(request, cancel);
                record(inner, file, path, recorder.redactor.clone())
            }
        }
    }
}

fn binding_key(binding: &ResolvedBinding) -> BindingKey {
    BindingKey {
        provider: binding.provider,
        model: binding.model.clone(),
        reasoning_effort: binding.reasoning_effort,
        data_policy: binding.data_policy,
        context: binding.context,
    }
}

/// Object keys in sorted order at every level, so the hash doesn't depend on how a
/// schema or tool definition happened to be written.
fn sorted(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let map: BTreeMap<String, Value> =
                map.into_iter().map(|(k, v)| (k, sorted(v))).collect();
            Value::Object(map.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

fn replay(path: &Path, key: &str, binding: &BindingKey, cancel: CancellationToken) -> EventStream {
    let Ok(bytes) = std::fs::read(path) else {
        let model = format!("{}:{}", provider_name(binding.provider), binding.model);
        panic!(
            "no cassette for this model call (key {key}); record it with \
             `cargo xtask cassettes record --suite <suite> --model {model}` and commit it"
        );
    };
    let events = match serde_json::from_slice::<File>(&bytes) {
        Ok(file) if file.v == VERSION && file.key == key => file.events,
        _ => return failed("the cassette file is malformed or for another call"),
    };
    // Like a live call: once canceled, one `Canceled` and the stream ends.
    let mut events = events.into_iter();
    let mut done = false;
    futures_util::stream::poll_fn(move |_| {
        if done {
            return std::task::Poll::Ready(None);
        }
        if cancel.is_cancelled() {
            done = true;
            return std::task::Poll::Ready(Some(Err(ProviderError::new(ErrorKind::Canceled))));
        }
        std::task::Poll::Ready(events.next().map(Ok))
    })
    .boxed()
}

/// Buffers the whole response, writes the file if the call finished cleanly, then
/// plays the response back. ponytail: buffered, so a recorded call has no streaming
/// timing; recording is a manual step and replay is what tests time.
fn record(inner: EventStream, mut file: File, path: PathBuf, redactor: Redactor) -> EventStream {
    futures_util::stream::once(async move {
        let mut items: Vec<Result<Event, ProviderError>> = inner.collect().await;
        let finished = matches!(items.last(), Some(Ok(Event::Finish { .. })));
        if !finished || items.iter().any(Result::is_err) {
            // A failed call isn't a response to replay; pass it through unrecorded.
            return items;
        }
        file.events = items.iter().filter_map(|e| e.clone().ok()).collect();
        let mut text = serde_json::to_string_pretty(&file).expect("cassette serializes");
        text.push('\n');
        // Scan what the file means, not its escaped JSON (which doubles a Windows
        // path's backslashes), and the deltas joined, since a key can span chunks.
        // The redactor's shapes cover `Bearer <token>`; a header name is refused alone.
        let meaning = plain_text(&file);
        let leaks = redactor.redact(&meaning) != meaning
            || meaning.to_ascii_lowercase().contains("authorization");
        let written = !leaks
            && path.parent().is_some_and(|dir| std::fs::create_dir_all(dir).is_ok())
            && std::fs::write(&path, text).is_ok();
        if !written {
            items.pop();
            items.push(Err(ProviderError::new(ErrorKind::InvalidResponse(
                "the cassette wasn't written: it would hold a secret or a local path, or the file couldn't be saved",
            ))));
        }
        items
    })
    .flat_map(futures_util::stream::iter)
    .boxed()
}

/// Every string in the file and in tool-call arguments, decoded, one per line, then
/// the text deltas joined.
fn plain_text(file: &File) -> String {
    fn strings(value: &serde_json::Value, out: &mut String) {
        match value {
            serde_json::Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
            serde_json::Value::Object(map) => map.values().for_each(|v| strings(v, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    let mut joined = String::new();
    strings(
        &serde_json::to_value(file).expect("cassette serializes"),
        &mut out,
    );
    for event in &file.events {
        match event {
            Event::TextDelta { text } => joined.push_str(text),
            // Arguments are JSON inside the file's JSON: decode their strings too.
            Event::ToolCall(call) => {
                if let Ok(arguments) = serde_json::from_str(&call.arguments) {
                    strings(&arguments, &mut out);
                }
            }
            _ => {}
        }
    }
    out.push_str(&joined);
    out
}

fn failed(rule: &'static str) -> EventStream {
    let error = ProviderError::new(ErrorKind::InvalidResponse(rule));
    futures_util::stream::iter([Err(error)]).boxed()
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Openrouter => "openrouter",
        Provider::Ollama => "ollama",
    }
}
