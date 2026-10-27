// SPDX-License-Identifier: AGPL-3.0-or-later

//! OpenAI Chat Completions over SSE, as OpenRouter serves it (bring your own key).
//! Wire facts are from OpenRouter's API reference: `streaming.md` (comment lines,
//! `[DONE]`, mid-stream `error` events under HTTP 200), `errors.md` (status codes,
//! `Retry-After`, the in-flight-budget 402) and `provider-routing.md` (`data_collection`,
//! `zdr`). Only the fields used here are typed; unknown ones are ignored.
//!
//! Nothing from a response body reaches an error: bodies are read only to pick an
//! [`ErrorKind`], never kept.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::StreamExt;
use gitcko_model::stream::{Decode, MAX_BODY, Sse, decode};
use gitcko_model::{
    ErrorKind, Event, EventStream, FinishReason, Message, ModelProvider, ProviderError, Request,
    ResolvedBinding, TimeoutKind, ToolCall, Usage,
};
use gitcko_protocol::config::DataPolicy;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// OpenRouter's API root.
pub const OPENROUTER: &str = "https://openrouter.ai/api/v1";

/// A binding's calls to an OpenAI-compatible endpoint.
pub struct OpenRouter {
    client: reqwest::Client,
    url: String,
    binding: ResolvedBinding,
}

impl OpenRouter {
    /// Calls go to [`OPENROUTER`].
    pub fn new(binding: ResolvedBinding) -> Result<OpenRouter, ProviderError> {
        OpenRouter::at(binding, OPENROUTER)
    }

    /// Calls go to `base` (`…/chat/completions` is appended), so tests can point at a
    /// local mock server.
    pub fn at(binding: ResolvedBinding, base: &str) -> Result<OpenRouter, ProviderError> {
        // ponytail: reqwest is built without a crypto provider (no aws-lc-rs), so ring
        // is installed as the process default; an already-installed one is kept.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .build()
            .map_err(|_| ProviderError::new(ErrorKind::Connect))?;
        Ok(OpenRouter {
            client,
            url: format!("{}/chat/completions", base.trim_end_matches('/')),
            binding,
        })
    }
}

impl ModelProvider for OpenRouter {
    fn stream(&self, request: Request, cancel: CancellationToken) -> EventStream {
        let Some(key) = &self.binding.credential else {
            return failed(ProviderError::new(ErrorKind::Auth));
        };
        let send = self
            .client
            .post(&self.url)
            .bearer_auth(key.expose())
            .json(&body(&self.binding, &request))
            .send();
        let canceled = cancel.clone();
        let open = async move {
            let response = send.await.map_err(transport)?;
            let status = response.status().as_u16();
            if !response.status().is_success() {
                let retry_after = retry_after(response.headers());
                let body = read_capped(response).await;
                return Err(status_error(status, retry_after, &body));
            }
            let sse = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/event-stream"));
            if !sse {
                return Err(ProviderError::new(ErrorKind::InvalidResponse(
                    "the response is not an event stream",
                )));
            }
            let bytes = response.bytes_stream().map(|r| r.map_err(transport));
            Ok(decode(bytes, Decoder::default(), cancel))
        };
        futures_util::stream::once(async move {
            tokio::select! {
                () = canceled.cancelled() => failed(ProviderError::new(ErrorKind::Canceled)),
                opened = open => opened.unwrap_or_else(failed),
            }
        })
        .flatten()
        .boxed()
    }
}

fn failed(error: ProviderError) -> EventStream {
    futures_util::stream::iter([Err(error)]).boxed()
}

/// A transport failure, by kind only: reqwest's message can hold the URL.
/// No reqwest timeout is set (the router owns timeouts), so every failure here is a
/// refused, reset or dropped connection.
fn transport(_: reqwest::Error) -> ProviderError {
    ProviderError::new(ErrorKind::Connect)
}

/// `Retry-After` in seconds. OpenRouter sends seconds; an HTTP date is ignored.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    value.parse().ok().map(Duration::from_secs)
}

/// An error body, up to [`MAX_BODY`]; past that, or on a read error, what was read.
async fn read_capped(mut response: reqwest::Response) -> Vec<u8> {
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if body.len() + chunk.len() > MAX_BODY {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    body
}

#[derive(Deserialize)]
struct ErrorBody {
    error: WireError,
}

#[derive(Deserialize)]
struct WireError {
    #[serde(default)]
    code: Value,
    #[serde(default)]
    metadata: Option<Value>,
}

impl WireError {
    fn limit_source(&self) -> Option<&str> {
        self.metadata.as_ref()?.get("limit_source")?.as_str()
    }
}

/// Maps an HTTP error (OpenRouter `errors.md`) to a kind. The body is only looked
/// at for `limit_source`, which tells a wait-and-retry 402 from an empty account.
pub fn status_error(status: u16, retry_after: Option<Duration>, body: &[u8]) -> ProviderError {
    let error = serde_json::from_slice::<ErrorBody>(body)
        .ok()
        .map(|b| b.error);
    let limit_source = error.as_ref().and_then(WireError::limit_source);
    ProviderError {
        kind: kind_for(status, retry_after, limit_source),
        status: Some(status),
    }
}

fn kind_for(status: u16, retry_after: Option<Duration>, limit_source: Option<&str>) -> ErrorKind {
    match status {
        // ponytail: OpenRouter has no typed context-length code; it is a 400 like any
        // other bad request. Neither is retried, and both fall back.
        400 => ErrorKind::BadRequest,
        401 => ErrorKind::Auth,
        402 if limit_source == Some("openrouter_in_flight_budget") => {
            ErrorKind::RateLimited { retry_after }
        }
        402 => ErrorKind::InsufficientCredits,
        // Guardrail, workspace and key budgets are 403s that name their limit.
        403 if limit_source.is_some() => ErrorKind::InsufficientCredits,
        403 => ErrorKind::ContentPolicy,
        408 => ErrorKind::Timeout(TimeoutKind::Total),
        429 => ErrorKind::RateLimited { retry_after },
        // "No available provider": honour its `Retry-After` when it sends one.
        503 if retry_after.is_some() => ErrorKind::RateLimited { retry_after },
        503 => ErrorKind::Overloaded,
        500..=599 => ErrorKind::Server,
        _ => ErrorKind::BadRequest,
    }
}

/// The request body. Temperature is dropped when a reasoning effort is set, since
/// reasoning models reject it; `data_policy` becomes OpenRouter's provider filter.
pub fn body(binding: &ResolvedBinding, request: &Request) -> Value {
    let mut body = json!({
        "model": binding.model,
        "messages": request.messages.iter().map(message).collect::<Vec<_>>(),
        "stream": true,
        "max_tokens": request.max_output,
    });
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters,
                }})
            })
            .collect();
    }
    if let Some(format) = &request.response_format {
        body["response_format"] = json!({"type": "json_schema", "json_schema": {
            "name": format.name, "strict": true, "schema": format.schema,
        }});
    }
    match request.reasoning_effort.or(binding.reasoning_effort) {
        Some(effort) => body["reasoning"] = json!({ "effort": effort }),
        None => {
            if let Some(t) = request.temperature {
                body["temperature"] = json!(t);
            }
        }
    }
    match binding.data_policy.unwrap_or(DataPolicy::DenyCollection) {
        DataPolicy::DenyCollection => body["provider"] = json!({"data_collection": "deny"}),
        DataPolicy::Zdr => body["provider"] = json!({"zdr": true}),
        DataPolicy::ProviderDefault => {}
    }
    body
}

fn message(message: &Message) -> Value {
    match message {
        Message::System { text } => json!({"role": "system", "content": text}),
        Message::User { text } => json!({"role": "user", "content": text}),
        Message::Assistant { text, tool_calls } if tool_calls.is_empty() => {
            json!({"role": "assistant", "content": text})
        }
        Message::Assistant { text, tool_calls } => json!({
            "role": "assistant",
            "content": text,
            "tool_calls": tool_calls.iter().map(|c| json!({
                "id": c.id, "type": "function",
                "function": {"name": c.name, "arguments": c.arguments},
            })).collect::<Vec<_>>(),
        }),
        Message::Tool { call_id, text } => {
            json!({"role": "tool", "tool_call_id": call_id, "content": text})
        }
    }
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<WireUsage>,
    error: Option<WireError>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct Delta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolDelta>,
}

#[derive(Deserialize)]
struct ToolDelta {
    #[serde(default)]
    index: u64,
    id: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    cost: Option<f64>,
}

/// SSE chunks → events. Tool calls stream as fragments keyed by `index` and are
/// emitted whole at `[DONE]`; usage comes in the last chunk, and
/// `Finish` only after `[DONE]`, so a cut-off stream is an error, not a short answer.
#[derive(Default)]
pub struct Decoder {
    sse: Sse,
    tools: BTreeMap<u64, ToolCall>,
    tool_bytes: usize,
    finish: Option<FinishReason>,
    usage: Option<Usage>,
}

fn invalid(rule: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::InvalidResponse(rule))
}

impl Decoder {
    fn chunk(&mut self, data: &str, events: &mut Vec<Event>) -> Result<(), ProviderError> {
        let chunk: Chunk = serde_json::from_str(data).map_err(|_| invalid("a malformed chunk"))?;
        if let Some(error) = chunk.error {
            // Mid-stream errors carry the HTTP status they would have had as `code`.
            let status = error.code.as_u64().and_then(|c| u16::try_from(c).ok());
            let kind = match status {
                Some(status) => kind_for(status, None, error.limit_source()),
                None => ErrorKind::Server,
            };
            return Err(ProviderError { kind, status });
        }
        for choice in chunk.choices {
            if let Some(text) = choice.delta.content.filter(|t| !t.is_empty()) {
                events.push(Event::TextDelta { text });
            }
            for part in choice.delta.tool_calls {
                let call = self.tools.entry(part.index).or_insert_with(|| ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
                let function = part.function.unwrap_or(FunctionDelta {
                    name: None,
                    arguments: None,
                });
                for (into, from) in [
                    (&mut call.id, part.id),
                    (&mut call.name, function.name),
                    (&mut call.arguments, function.arguments),
                ] {
                    let from = from.unwrap_or_default();
                    self.tool_bytes += from.len();
                    into.push_str(&from);
                }
                if self.tool_bytes > MAX_BODY {
                    return Err(ProviderError::new(ErrorKind::TooLarge("tool calls")));
                }
            }
            if let Some(reason) = choice.finish_reason {
                self.finish = Some(match reason.as_str() {
                    "stop" => FinishReason::Stop,
                    "length" => FinishReason::Length,
                    "tool_calls" => FinishReason::ToolCalls,
                    "content_filter" => FinishReason::ContentFilter,
                    "error" => return Err(ProviderError::new(ErrorKind::Server)),
                    _ => return Err(invalid("an unknown finish reason")),
                });
            }
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(Usage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                cost_usd: usage.cost,
            });
        }
        Ok(())
    }

    fn flush_tools(&mut self, events: &mut Vec<Event>) -> Result<(), ProviderError> {
        for (_, call) in std::mem::take(&mut self.tools) {
            if call.name.is_empty() {
                return Err(invalid("a tool call without a name"));
            }
            events.push(Event::ToolCall(call));
        }
        Ok(())
    }
}

impl Decode for Decoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Event>, ProviderError> {
        let mut events = Vec::new();
        for data in self.sse.push(bytes)? {
            if data == "[DONE]" {
                self.flush_tools(&mut events)?;
                let reason = self
                    .finish
                    .ok_or_else(|| invalid("no finish reason before [DONE]"))?;
                events.extend(self.usage.take().map(Event::Usage));
                events.push(Event::Finish { reason });
                // `decode` stops reading at `Finish`.
                break;
            }
            self.chunk(&data, &mut events)?;
        }
        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<Event>, ProviderError> {
        Err(invalid("the stream ended before [DONE]"))
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn an_error_body_is_read_only_up_to_the_cap() {
        let huge = http::Response::new(vec![b'x'; super::MAX_BODY + 1]);
        assert!(super::read_capped(huge.into()).await.is_empty());
        let small = http::Response::new(b"{}".to_vec());
        assert_eq!(super::read_capped(small.into()).await, b"{}");
    }
}
