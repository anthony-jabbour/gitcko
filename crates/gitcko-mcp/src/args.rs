// SPDX-License-Identifier: AGPL-3.0-or-later

//! Argument checks for the four tools (MCP_SPEC §3.1).
//!
//! Schema failures are `INVALID_REQUEST`. `tier` other than `base`, and any
//! `instructions` string, are `CAPABILITY_UNAVAILABLE`. Messages are fixed: they
//! never echo the argument.

use std::path::Path;

use gitcko_domain::{RepoPath, ReviewId};
use serde_json::{Map, Value};

use crate::paths::check_path;

/// The only tools this build advertises, in list order.
pub const TOOLS: [&str; 4] = [
    "gitcko_review",
    "gitcko_review_status",
    "gitcko_get_review",
    "gitcko_list_findings",
];

pub struct ArgError {
    pub code: &'static str,
    pub message: &'static str,
    pub retryable: bool,
}

fn invalid() -> ArgError {
    ArgError {
        code: "INVALID_REQUEST",
        message: "the arguments do not match the tool schema",
        retryable: false,
    }
}

fn capability(message: &'static str) -> ArgError {
    ArgError {
        code: "CAPABILITY_UNAVAILABLE",
        message,
        retryable: false,
    }
}

pub enum ToolArgs {
    Review {
        base: Option<String>,
        head: Option<String>,
        paths: Vec<RepoPath>,
    },
    Status {
        review_id: Option<String>,
        head: Option<String>,
    },
    Get {
        review_id: String,
    },
    List {
        review_id: Option<String>,
        addressed: Option<bool>,
        has_suggestion: Option<bool>,
        severity: Option<Vec<String>>,
        path: Option<RepoPath>,
        limit: usize,
        cursor: Option<String>,
    },
}

/// Parse one JSON document and check it as every tool's arguments. Never panics.
/// The fuzz target calls this. `None` for the root skips the symlink walk.
pub fn check_mcp_args(data: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(data) else {
        return;
    };
    let Some(object) = value.as_object() else {
        return;
    };
    for name in TOOLS {
        let _ = validate(name, object, None);
    }
}

pub fn validate(
    name: &str,
    object: &Map<String, Value>,
    root: Option<&Path>,
) -> Result<ToolArgs, ArgError> {
    match name {
        "gitcko_review" => review_args(object, root),
        "gitcko_review_status" => status_args(object),
        "gitcko_get_review" => get_args(object),
        "gitcko_list_findings" => list_args(object, root),
        _ => Err(invalid()),
    }
}

fn review_args(object: &Map<String, Value>, root: Option<&Path>) -> Result<ToolArgs, ArgError> {
    only_keys(object, &["base", "head", "tier", "instructions", "paths"])?;
    let base = optional_string(object, "base", 1, 255)?.map(str::to_string);
    let head = optional_string(object, "head", 1, 255)?.map(str::to_string);
    let paths = path_list(object, root)?;
    tier(object)?;
    if optional_string(object, "instructions", 0, 2000)?.is_some() {
        return Err(capability("instructions are not available in this build"));
    }
    Ok(ToolArgs::Review { base, head, paths })
}

fn status_args(object: &Map<String, Value>) -> Result<ToolArgs, ArgError> {
    only_keys(object, &["review_id", "head"])?;
    let review_id = review_id(object, false)?;
    let head = optional_string(object, "head", 1, 255)?.map(str::to_string);
    Ok(ToolArgs::Status { review_id, head })
}

fn get_args(object: &Map<String, Value>) -> Result<ToolArgs, ArgError> {
    only_keys(object, &["review_id"])?;
    let review_id = review_id(object, true)?.ok_or_else(invalid)?;
    Ok(ToolArgs::Get { review_id })
}

fn list_args(object: &Map<String, Value>, root: Option<&Path>) -> Result<ToolArgs, ArgError> {
    only_keys(
        object,
        &[
            "review_id",
            "addressed",
            "has_suggestion",
            "severity",
            "path",
            "limit",
            "cursor",
        ],
    )?;
    let review_id = review_id(object, false)?;
    let addressed = optional_bool(object, "addressed")?;
    let has_suggestion = optional_bool(object, "has_suggestion")?;
    let severity = severity(object)?;
    let path = one_path(object, root)?;
    let limit = limit(object)?;
    let cursor = optional_string(object, "cursor", 0, 2048)?.map(str::to_string);
    Ok(ToolArgs::List {
        review_id,
        addressed,
        has_suggestion,
        severity,
        path,
        limit,
        cursor,
    })
}

fn only_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<(), ArgError> {
    if object.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(invalid())
    }
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    min: usize,
    max: usize,
) -> Result<Option<&'a str>, ArgError> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(text)) if (min..=max).contains(&text.chars().count()) => Ok(Some(text)),
        Some(_) => Err(invalid()),
    }
}

fn optional_bool(object: &Map<String, Value>, key: &str) -> Result<Option<bool>, ArgError> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(invalid()),
    }
}

fn tier(object: &Map<String, Value>) -> Result<(), ArgError> {
    match object.get("tier") {
        None => Ok(()),
        Some(Value::String(value)) if value == "base" => Ok(()),
        Some(Value::String(value)) if value == "plus" || value == "max" || value == "auto" => {
            Err(capability("tier is not available in this build"))
        }
        Some(_) => Err(invalid()),
    }
}

fn review_id(object: &Map<String, Value>, required: bool) -> Result<Option<String>, ArgError> {
    match optional_string(object, "review_id", 1, 128)? {
        None if required => Err(invalid()),
        None => Ok(None),
        Some(id) => {
            if id.parse::<ReviewId>().is_err() {
                return Err(invalid());
            }
            Ok(Some(id.to_string()))
        }
    }
}

fn path_list(object: &Map<String, Value>, root: Option<&Path>) -> Result<Vec<RepoPath>, ArgError> {
    match object.get("paths") {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) if items.len() <= 256 => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(text) = item.as_str() else {
                    return Err(invalid());
                };
                if text.is_empty() || text.chars().count() > 1024 {
                    return Err(invalid());
                }
                out.push(check_path(root, text).map_err(|_| invalid())?);
            }
            Ok(out)
        }
        Some(_) => Err(invalid()),
    }
}

fn one_path(
    object: &Map<String, Value>,
    root: Option<&Path>,
) -> Result<Option<RepoPath>, ArgError> {
    match optional_string(object, "path", 1, 1024)? {
        None => Ok(None),
        Some(text) => Ok(Some(check_path(root, text).map_err(|_| invalid())?)),
    }
}

fn severity(object: &Map<String, Value>) -> Result<Option<Vec<String>>, ArgError> {
    match object.get("severity") {
        None => Ok(None),
        Some(Value::Array(items)) if (1..=8).contains(&items.len()) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(text) = item.as_str() else {
                    return Err(invalid());
                };
                if text != "P0" && text != "P1" && text != "P2" {
                    return Err(invalid());
                }
                out.push(text.to_string());
            }
            Ok(Some(out))
        }
        Some(_) => Err(invalid()),
    }
}

fn limit(object: &Map<String, Value>) -> Result<usize, ArgError> {
    match object.get("limit") {
        None => Ok(50),
        Some(Value::Number(number)) => {
            let Some(value) = number.as_u64() else {
                return Err(invalid());
            };
            if (1..=100).contains(&value) {
                Ok(usize::try_from(value).unwrap_or(50))
            } else {
                Err(invalid())
            }
        }
        Some(_) => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_mcp_args_does_not_panic_on_hostile_payloads() {
        check_mcp_args(b"");
        check_mcp_args(b"null");
        check_mcp_args(br#"{"paths":["../canary","/etc/canary"],"tier":"plus"}"#);
        check_mcp_args(&[0xff, 0xfe]);
        let huge = format!(r#"{{"head":"{}"}}"#, "a".repeat(10_000));
        check_mcp_args(huge.as_bytes());
    }
}
