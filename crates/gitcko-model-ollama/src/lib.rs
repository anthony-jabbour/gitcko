// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ollama's native `/api/chat` over NDJSON. Native rather than its OpenAI-compatible
//! `/v1`, because only the native API sets `num_ctx` (Ollama's default context is 4k
//! on most laptops, under every class floor) and reports `remote_host`, which marks a
//! cloud model served through a local server (ollama/ollama `docs/api.md`,
//! `api/types.go`).
//!
//! Loopback is not proof of local: before each chat, `/api/show` must report no
//! `remote_host` for an on-device binding, and a chat chunk that carries one ends the
//! call. Nothing from a response body reaches an error.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use futures_util::StreamExt;
use gitcko_model::stream::{Decode, MAX_BODY, Ndjson, decode};
use gitcko_model::{
    DestinationClass, ErrorKind, Event, EventStream, FinishReason, Message, ModelProvider,
    ProviderError, Request, ResolvedBinding, ToolCall, Usage,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// A binding's calls to an Ollama server.
pub struct Ollama {
    client: reqwest::Client,
    base: String,
    binding: ResolvedBinding,
}

impl Ollama {
    /// Calls go to the binding's endpoint. A `localhost` endpoint is pinned to the
    /// loopback addresses, so DNS can't move it, and no proxy is ever used: a proxy
    /// would carry an on-device call off the machine.
    pub fn new(binding: ResolvedBinding) -> Result<Ollama, ProviderError> {
        let Some(origin) = &binding.endpoint else {
            return Err(ProviderError::new(ErrorKind::Connect));
        };
        let port = binding.destination.port;
        // ponytail: reqwest is built without a crypto provider (no aws-lc-rs), so ring
        // is installed as the process default; an already-installed one is kept.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .no_proxy()
            .resolve_to_addrs(
                "localhost",
                &[
                    SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                    SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
                ],
            )
            .build()
            .map_err(|_| ProviderError::new(ErrorKind::Connect))?;
        let scheme = if origin.https { "https" } else { "http" };
        Ok(Ollama {
            client,
            base: format!("{scheme}://{}:{port}", origin.host),
            binding,
        })
    }
}

impl ModelProvider for Ollama {
    fn stream(&self, request: Request, cancel: CancellationToken) -> EventStream {
        let on_device = self.binding.destination.class == DestinationClass::OnDevice;
        let show = self
            .client
            .post(format!("{}/api/show", self.base))
            .json(&json!({"model": self.binding.model}))
            .send();
        let chat = self
            .client
            .post(format!("{}/api/chat", self.base))
            .json(&body(&self.binding, &request));
        let canceled = cancel.clone();
        let open = async move {
            let shown = show.await.map_err(transport)?;
            let status = shown.status().as_u16();
            let shown = read_capped(shown).await;
            if !(200..300).contains(&status) {
                return Err(status_error(status));
            }
            let shown: Show = serde_json::from_slice(&shown)
                .map_err(|_| invalid("a malformed /api/show response"))?;
            if on_device && shown.remote_host.is_some_and(|h| !h.is_empty()) {
                return Err(ProviderError::new(ErrorKind::LocalityViolation));
            }
            let response = chat.send().await.map_err(transport)?;
            let status = response.status().as_u16();
            if !response.status().is_success() {
                return Err(status_error(status));
            }
            let bytes = response.bytes_stream().map(|r| r.map_err(transport));
            Ok(decode(bytes, Decoder::new(on_device), cancel))
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

fn invalid(rule: &'static str) -> ProviderError {
    ProviderError::new(ErrorKind::InvalidResponse(rule))
}

/// No reqwest timeout is set (the router owns timeouts), so every failure here is a
/// refused, reset or dropped connection.
fn transport(_: reqwest::Error) -> ProviderError {
    ProviderError::new(ErrorKind::Connect)
}

/// A body up to [`MAX_BODY`]; past that, or on a read error, what was read.
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

/// Ollama's statuses: 400 for a bad request, 404 for a model that isn't pulled,
/// 401/403 for a cloud model without sign-in, 503 when its queue is full. Its error
/// body is a bare message, so it is never read.
pub fn status_error(status: u16) -> ProviderError {
    let kind = match status {
        401 | 403 => ErrorKind::Auth,
        429 => ErrorKind::RateLimited { retry_after: None },
        503 => ErrorKind::Overloaded,
        500..=599 => ErrorKind::Server,
        _ => ErrorKind::BadRequest,
    };
    ProviderError {
        kind,
        status: Some(status),
    }
}

#[derive(Deserialize)]
struct Show {
    remote_host: Option<String>,
}

/// The `/api/chat` body: `num_ctx` from the binding, `num_predict` from the budget,
/// `format` for structured output and `think` for a reasoning effort.
pub fn body(binding: &ResolvedBinding, request: &Request) -> Value {
    let mut options = json!({ "num_predict": request.max_output });
    if let Some(context) = binding.context {
        options["num_ctx"] = json!(context);
    }
    if let Some(t) = request.temperature {
        options["temperature"] = json!(t);
    }
    let mut body = json!({
        "model": binding.model,
        "messages": request.messages.iter().map(|m| message(m, &request.messages)).collect::<Vec<_>>(),
        "stream": true,
        "options": options,
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
        body["format"] = format.schema.clone();
    }
    if let Some(effort) = request.reasoning_effort.or(binding.reasoning_effort) {
        body["think"] = json!(effort);
    }
    body
}

fn message(message: &Message, all: &[Message]) -> Value {
    match message {
        Message::System { text } => json!({"role": "system", "content": text}),
        Message::User { text } => json!({"role": "user", "content": text}),
        Message::Assistant { text, tool_calls } => {
            let mut wire = json!({"role": "assistant", "content": text});
            if !tool_calls.is_empty() {
                wire["tool_calls"] = tool_calls
                    .iter()
                    .map(|c| {
                        // Ollama takes arguments as an object; text that isn't JSON
                        // goes as a string rather than being dropped.
                        let arguments = serde_json::from_str::<Value>(&c.arguments)
                            .unwrap_or_else(|_| Value::String(c.arguments.clone()));
                        json!({"function": {"name": c.name, "arguments": arguments}})
                    })
                    .collect();
            }
            wire
        }
        // Ollama matches a tool result by the tool's name, not a call id.
        Message::Tool { call_id, text } => {
            let name = all.iter().find_map(|m| match m {
                Message::Assistant { tool_calls, .. } => tool_calls
                    .iter()
                    .find(|c| c.id == *call_id)
                    .map(|c| c.name.as_str()),
                _ => None,
            });
            json!({"role": "tool", "content": text, "tool_name": name.unwrap_or_default()})
        }
    }
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    message: Option<WireMessage>,
    #[serde(default)]
    done: bool,
    done_reason: Option<String>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
    error: Option<Value>,
    remote_host: Option<String>,
}

#[derive(Deserialize)]
struct WireMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Deserialize)]
struct WireToolCall {
    id: Option<String>,
    function: WireFunction,
}

#[derive(Deserialize)]
struct WireFunction {
    name: String,
    #[serde(default)]
    arguments: Value,
}

/// NDJSON chunks → events. Ollama sends each tool call whole and has no `[DONE]`:
/// the chunk with `done: true` carries the counts, and a stream that ends without
/// one was cut off.
pub struct Decoder {
    lines: Ndjson,
    on_device: bool,
    tool_calls: usize,
}

impl Decoder {
    /// `on_device`: a chunk naming a `remote_host` is a locality violation.
    pub fn new(on_device: bool) -> Decoder {
        Decoder {
            lines: Ndjson::default(),
            on_device,
            tool_calls: 0,
        }
    }

    fn line(&mut self, line: &str, events: &mut Vec<Event>) -> Result<bool, ProviderError> {
        let chunk: Chunk = serde_json::from_str(line).map_err(|_| invalid("a malformed chunk"))?;
        if self.on_device && chunk.remote_host.is_some_and(|h| !h.is_empty()) {
            return Err(ProviderError::new(ErrorKind::LocalityViolation));
        }
        if chunk.error.is_some() {
            return Err(ProviderError::new(ErrorKind::Server));
        }
        if let Some(message) = chunk.message {
            if !message.content.is_empty() {
                events.push(Event::TextDelta {
                    text: message.content,
                });
            }
            for call in message.tool_calls {
                self.tool_calls += 1;
                events.push(Event::ToolCall(ToolCall {
                    // Older servers send no id; the router only needs it unique.
                    id: call
                        .id
                        .unwrap_or_else(|| format!("call_{}", self.tool_calls)),
                    name: call.function.name,
                    arguments: call.function.arguments.to_string(),
                }));
            }
        }
        if !chunk.done {
            return Ok(false);
        }
        let reason = match chunk.done_reason.as_deref() {
            Some("length") => FinishReason::Length,
            _ if self.tool_calls > 0 => FinishReason::ToolCalls,
            _ => FinishReason::Stop,
        };
        events.push(Event::Usage(Usage {
            input_tokens: chunk.prompt_eval_count,
            output_tokens: chunk.eval_count,
            cost_usd: None,
        }));
        events.push(Event::Finish { reason });
        Ok(true)
    }
}

impl Decode for Decoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Event>, ProviderError> {
        let mut events = Vec::new();
        for line in self.lines.push(bytes)? {
            if self.line(&line, &mut events)? {
                break;
            }
        }
        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<Event>, ProviderError> {
        let mut events = Vec::new();
        if let Some(line) = self.lines.finish()?
            && self.line(&line, &mut events)?
        {
            return Ok(events);
        }
        Err(invalid("the stream ended before done"))
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_body_is_read_only_up_to_the_cap() {
        let huge = http::Response::new(vec![b'x'; super::MAX_BODY + 1]);
        assert!(super::read_capped(huge.into()).await.is_empty());
    }
}
