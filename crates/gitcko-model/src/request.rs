// SPDX-License-Identifier: AGPL-3.0-or-later

//! What goes to a provider and what comes back, in provider-neutral terms. Adapters
//! translate these to and from their wire format. `Serialize` because cassette keys
//! are a hash of the request's canonical JSON (tests/cassettes/README.md).

use gitcko_protocol::config::ReasoningEffort;
use serde::{Deserialize, Serialize};

/// One model call. The binding (which model, where) is not here: the router picks it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub messages: Vec<Message>,
    /// Tools the model may call. Empty means none.
    pub tools: Vec<Tool>,
    /// Ask for JSON matching this schema instead of free text.
    pub response_format: Option<ResponseFormat>,
    /// Set by the router from the run budget, so the provider enforces it too.
    pub max_output: u32,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Dropped by the adapter for models that don't accept it (MODEL_ROUTING §3.1).
    pub temperature: Option<f32>,
}

/// One turn of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        text: String,
    },
    User {
        text: String,
    },
    Assistant {
        text: String,
        tool_calls: Vec<ToolCall>,
    },
    /// A tool's answer to the call with this id.
    Tool {
        call_id: String,
        text: String,
    },
}

/// A function the model may call, described by a JSON schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A named JSON schema for structured output (OpenAI `json_schema`, Ollama `format`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseFormat {
    pub name: String,
    pub schema: serde_json::Value,
}

/// A complete tool call. Adapters assemble streamed fragments before emitting one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON text as the model wrote it; the caller validates it.
    pub arguments: String,
}

/// One item of a response stream. A stream ends with `Finish`, or with an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    TextDelta { text: String },
    ToolCall(ToolCall),
    Usage(Usage),
    Finish { reason: FinishReason },
}

/// What the provider reported. `None` means it didn't say, which is recorded as
/// `unknown` and never as zero (MODEL_ROUTING §12).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// Why the model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    /// Hit `max_output`.
    Length,
    ToolCalls,
    ContentFilter,
}
