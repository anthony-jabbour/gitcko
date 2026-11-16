// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tool list for Server A. The input objects are MCP_SPEC §3.1, copied so a later
//! F-12 tool can be added without changing these four.
//!
//! https://docs.rs/rmcp/3.10.0/rmcp/model/struct.Tool.html

use std::sync::Arc;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};

use crate::args::TOOLS;

const REVIEW: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "base": { "type": "string", "minLength": 1, "maxLength": 255 },
    "head": { "type": "string", "minLength": 1, "maxLength": 255 },
    "tier": { "type": "string", "enum": ["base", "plus", "max", "auto"], "default": "base" },
    "instructions": { "type": "string", "maxLength": 2000 },
    "paths": {
      "type": "array",
      "maxItems": 256,
      "items": { "type": "string", "minLength": 1, "maxLength": 1024 }
    }
  }
}"#;

const STATUS: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "review_id": { "type": "string", "minLength": 1, "maxLength": 128 },
    "head": { "type": "string", "minLength": 1, "maxLength": 255 }
  }
}"#;

const GET: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": { "review_id": { "type": "string", "minLength": 1, "maxLength": 128 } },
  "required": ["review_id"]
}"#;

const LIST: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "review_id": { "type": "string", "minLength": 1, "maxLength": 128 },
    "addressed": { "type": "boolean" },
    "has_suggestion": { "type": "boolean" },
    "severity": { "type": "array", "minItems": 1, "items": { "enum": ["P0", "P1", "P2"] } },
    "path": { "type": "string", "minLength": 1, "maxLength": 1024 },
    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 50 },
    "cursor": { "type": "string", "maxLength": 2048 }
  }
}"#;

const OUTPUT: &str = r#"{
  "type": "object",
  "additionalProperties": true,
  "required": ["schema_version", "status", "untrusted_content"],
  "properties": {
    "schema_version": { "const": "gitcko.mcp/1" },
    "status": { "enum": ["complete", "partial", "failed", "canceled", "superseded"] },
    "untrusted_content": { "type": "boolean" }
  }
}"#;

pub fn tool_list() -> Vec<Tool> {
    let output = schema(OUTPUT);
    let specs = [
        (
            TOOLS[0],
            "Review base…head through the local engine. This build never publishes the result.",
            REVIEW,
            false,
        ),
        (
            TOOLS[1],
            "Lifecycle, completeness, and addressed counts for a stored review or the current head.",
            STATUS,
            true,
        ),
        (
            TOOLS[2],
            "Stored review: summary, score, and the first page of findings.",
            GET,
            true,
        ),
        (
            TOOLS[3],
            "Findings for a stored review. fix_package is null until fix packages ship.",
            LIST,
            true,
        ),
    ];
    specs
        .into_iter()
        .map(|(name, description, input, read_only)| {
            let mut tool = Tool::new(name, description, schema(input));
            tool.output_schema = Some(Arc::clone(&output));
            tool.annotations = Some(annotations(read_only));
            tool
        })
        .collect()
}

fn annotations(read_only: bool) -> ToolAnnotations {
    if read_only {
        ToolAnnotations::new().read_only(true).open_world(false)
    } else {
        ToolAnnotations::new()
            .read_only(false)
            .destructive(false)
            .idempotent(false)
            .open_world(false)
    }
}

fn schema(text: &str) -> Arc<JsonObject> {
    let value: serde_json::Value = serde_json::from_str(text).expect("tool schema is valid json");
    Arc::new(value.as_object().expect("tool schema is an object").clone())
}
