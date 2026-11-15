// SPDX-License-Identifier: AGPL-3.0-or-later

//! Result bodies for the four tools: envelope (MCP_SPEC §4.1), status (§4.2),
//! and finding (§4.3). `fix_package` is always null (P5 C3). An incomplete review's
//! text starts with `INCOMPLETE review` so it cannot be read as a clean result.

use gitcko_protocol::event::{RunEvent, RunEventKind};
use gitcko_protocol::review::{Finding, Lifecycle, Review, ReviewStatus};
use gitcko_render::text;
use gitcko_storage::ReviewStore;
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Value, json};
use sha2::Digest;

use crate::paths::hex;

pub const PAGE: usize = 50;

pub fn tool_error(code: &str, message: &str, retryable: bool) -> CallToolResult {
    let value = json!({
        "code": code,
        "message": message,
        "retryable": retryable,
    });
    let mut result = CallToolResult::structured_error(value);
    result.content = vec![ContentBlock::text(format!("{code}: {message}"))];
    result
}

pub fn tool_ok(text: String, value: Value) -> CallToolResult {
    let mut result = CallToolResult::structured(value);
    result.content = vec![ContentBlock::text(text)];
    result
}

/// Text for a stored review. Partial reviews lead with `INCOMPLETE review (…)`.
pub fn review_text(review: &Review) -> String {
    let rendered = text(review);
    if review.status != ReviewStatus::Partial {
        return rendered;
    }
    let rest = rendered
        .split_once('\n')
        .map(|(_, rest)| rest)
        .unwrap_or("");
    if review.reason_codes.is_empty() {
        format!("INCOMPLETE review\n{rest}")
    } else {
        format!(
            "INCOMPLETE review ({})\n{rest}",
            review.reason_codes.join(", ")
        )
    }
}

pub fn review_body(
    review: &Review,
    root_id: &str,
    paths: &[String],
    findings: Vec<Value>,
    truncated: bool,
    next_cursor: Option<String>,
) -> Value {
    let mut body = envelope(review, root_id, truncated, next_cursor);
    let Some(object) = body.as_object_mut() else {
        return body;
    };
    object.insert("review_id".into(), json!(review.review_id));
    object.insert("tier".into(), json!("base"));
    object.insert("paths".into(), json!(paths));
    object.insert("summary".into(), json!(review.summary));
    object.insert("score".into(), json!(shown_score(review)));
    object.insert("findings".into(), Value::Array(findings));
    body
}

pub fn get_body(
    review: &Review,
    root_id: &str,
    findings: Vec<Value>,
    truncated: bool,
    next_cursor: Option<String>,
) -> Value {
    let mut body = envelope(review, root_id, truncated, next_cursor);
    let Some(object) = body.as_object_mut() else {
        return body;
    };
    object.insert("review_id".into(), json!(review.review_id));
    object.insert("tier".into(), json!("base"));
    object.insert("summary".into(), json!(review.summary));
    object.insert("score".into(), json!(shown_score(review)));
    object.insert("findings".into(), Value::Array(findings));
    body
}

pub fn status_body(
    review: &Review,
    root_id: &str,
    lifecycle: &str,
    total: usize,
    addressed: usize,
    head_moved: bool,
) -> Value {
    let mut body = envelope(review, root_id, false, None);
    insert_status(
        &mut body,
        &review.review_id,
        lifecycle,
        shown_score(review),
        total,
        addressed,
        head_moved,
    );
    body
}

/// A running or stale marker has no stored review. The envelope still uses §4.1.
#[allow(clippy::too_many_arguments)]
pub fn marker_body(
    review_id: &str,
    root_id: &str,
    base_sha: &str,
    head_sha: &str,
    envelope_status: &str,
    reason_codes: &[String],
    lifecycle: &str,
    head_moved: bool,
) -> Value {
    let mut body = json!({
        "schema_version": "gitcko.mcp/1",
        "status": envelope_status,
        "reason_codes": reason_codes,
        "repository": {
            "root_id": root_id,
            "base_sha": base_sha,
            "head_sha": head_sha,
        },
        "versions": {
            "engine": "0.0.0",
            "index": "none",
            "policy": "gitcko-score@1",
            "routing_policy": "gitcko-std@1",
        },
        "coverage": { "truncated": false, "next_cursor": null },
        "untrusted_content": true,
    });
    insert_status(&mut body, review_id, lifecycle, None, 0, 0, head_moved);
    body
}

pub fn list_body(
    review: &Review,
    root_id: &str,
    findings: Vec<Value>,
    limit: usize,
    truncated: bool,
    next_cursor: Option<String>,
) -> Value {
    let mut body = envelope(review, root_id, truncated, next_cursor);
    let Some(object) = body.as_object_mut() else {
        return body;
    };
    object.insert("review_id".into(), json!(review.review_id));
    object.insert("tier".into(), json!("base"));
    object.insert("limit".into(), json!(limit));
    object.insert("findings".into(), Value::Array(findings));
    body
}

pub fn finding_value(review: &Review, finding: &Finding, addressed: bool) -> Value {
    let (cause, impact) = split_body(&finding.body);
    json!({
        "finding_id": finding.id,
        "occurrence_id": format!("{}:{}", finding.id, review.review_id),
        "severity": serde_json::to_value(finding.severity).unwrap_or(Value::Null),
        "comment_type": serde_json::to_value(finding.comment_type).unwrap_or(Value::Null),
        "confidence": finding.confidence,
        "title": finding.title,
        "path": finding.path,
        "line_start": finding.start_line,
        "line_end": finding.end_line,
        "cause": cause,
        "impact": impact,
        "evidence": finding.evidence.iter().map(|item| json!({
            "kind": serde_json::to_value(item.kind).unwrap_or(Value::Null),
            "path": item.path,
            "line_start": item.start_line,
            "line_end": item.end_line,
        })).collect::<Vec<_>>(),
        "verification": serde_json::to_value(finding.verification).unwrap_or(Value::Null),
        "addressed": addressed,
        "has_suggestion": finding.suggestion.is_some(),
        "suggested_code": finding.suggestion,
        "fix_package": null,
        "linked_rule": finding.linked_rule,
        "pr": null,
    })
}

/// Latest later complete review of the same base decides. A later partial that drops
/// the finding does not count (P5 C13).
pub fn is_addressed(
    store: &ReviewStore,
    repo_id: &str,
    review: &Review,
    finding: &Finding,
) -> bool {
    let Ok(list) = store.list(repo_id) else {
        return false;
    };
    for summary in list {
        if summary.review_id.as_str() <= review.review_id.as_str() {
            continue;
        }
        if summary.base_sha != review.target.base_sha || summary.status != ReviewStatus::Complete {
            continue;
        }
        let Ok(later) = store.get(repo_id, &summary.review_id) else {
            continue;
        };
        return match later.findings.iter().find(|item| item.id == finding.id) {
            None => true,
            Some(item) => item.lifecycle == Lifecycle::Resolved,
        };
    }
    false
}

pub fn filter_key(
    addressed: Option<bool>,
    has_suggestion: Option<bool>,
    severity: Option<&[String]>,
    path: Option<&str>,
    limit: usize,
) -> String {
    let value = json!({
        "addressed": addressed,
        "has_suggestion": has_suggestion,
        "severity": severity,
        "path": path,
        "limit": limit,
    });
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    let digest = sha2::Sha256::digest(bytes);
    hex(&digest[..8])
}

pub fn parse_cursor(cursor: &str) -> Option<(String, String, usize)> {
    let mut parts = cursor.split('|');
    if parts.next()? != "gcko1" {
        return None;
    }
    let id = parts.next()?.to_string();
    let key = parts.next()?.to_string();
    let offset: usize = parts.next()?.parse().ok()?;
    if parts.next().is_some() || id.is_empty() || key.is_empty() {
        return None;
    }
    Some((id, key, offset))
}

pub fn cursor(review_id: &str, key: &str, offset: usize) -> String {
    format!("gcko1|{review_id}|{key}|{offset}")
}

pub fn lifecycle_of(status: ReviewStatus) -> &'static str {
    match status {
        ReviewStatus::Complete => "completed",
        ReviewStatus::Partial => "validating",
        ReviewStatus::Failed => "failed",
        ReviewStatus::Canceled => "canceled",
        ReviewStatus::Superseded => "superseded",
    }
}

fn shown_score(review: &Review) -> Option<u8> {
    if review.status == ReviewStatus::Complete {
        review.score
    } else {
        None
    }
}

fn envelope(review: &Review, root_id: &str, truncated: bool, next_cursor: Option<String>) -> Value {
    let routing = if review.versions.routing_policy.is_empty() {
        "gitcko-std@1".to_string()
    } else {
        review.versions.routing_policy.clone()
    };
    json!({
        "schema_version": "gitcko.mcp/1",
        "status": status_name(review.status),
        "reason_codes": review.reason_codes,
        "repository": {
            "root_id": root_id,
            "base_sha": review.target.base_sha,
            "head_sha": review.target.head_sha,
        },
        "versions": {
            "engine": review.versions.engine,
            "index": review.versions.index,
            "policy": review.versions.policy,
            "routing_policy": routing,
        },
        "coverage": {
            "truncated": truncated,
            "next_cursor": next_cursor,
        },
        "untrusted_content": true,
    })
}

fn insert_status(
    body: &mut Value,
    review_id: &str,
    lifecycle: &str,
    score: Option<u8>,
    total: usize,
    addressed: usize,
    head_moved: bool,
) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    object.insert("review_id".into(), json!(review_id));
    object.insert("lifecycle".into(), json!(lifecycle));
    object.insert("tier".into(), json!("base"));
    object.insert("score".into(), json!(score));
    object.insert("score_available".into(), json!(score.is_some()));
    object.insert("findings_total".into(), json!(total));
    object.insert("findings_addressed".into(), json!(addressed));
    object.insert(
        "completeness_text".into(),
        json!(format!("{addressed}/{total} Gitcko findings addressed")),
    );
    object.insert("has_new_commits_since_review".into(), json!(head_moved));
    object.insert("verification".into(), json!("not_run"));
}

fn status_name(status: ReviewStatus) -> &'static str {
    match status {
        ReviewStatus::Complete => "complete",
        ReviewStatus::Partial => "partial",
        ReviewStatus::Failed => "failed",
        ReviewStatus::Canceled => "canceled",
        ReviewStatus::Superseded => "superseded",
    }
}

fn split_body(body: &str) -> (String, String) {
    let Some((cause, rest)) = body.split_once("\n\nTrigger: ") else {
        return (body.to_string(), String::new());
    };
    let Some((_, impact)) = rest.split_once("\n\nImpact: ") else {
        return (body.to_string(), String::new());
    };
    (cause.to_string(), impact.to_string())
}

/// A progress line for a real stage. Other events are not a percentage and are not sent.
pub fn progress_note(event: &RunEvent) -> Option<String> {
    match &event.kind {
        RunEventKind::StepStarted { step } => Some(format!("stage: {step}")),
        RunEventKind::StepCompleted { step, counts, .. } => {
            if counts.is_empty() {
                Some(format!("stage: {step}"))
            } else {
                let extra = counts
                    .iter()
                    .map(|(name, count)| format!("{name}={count}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(format!("stage: {step} {extra}"))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{progress_note, split_body};
    use gitcko_protocol::event::{RunEvent, RunEventKind, RunLifecycle};

    #[test]
    fn progress_notes_name_real_stages_only() {
        let started = RunEvent {
            run_id: "run".into(),
            seq: 1,
            at: "2026-01-01T00:00:00Z".into(),
            kind: RunEventKind::StepStarted {
                step: "plan".into(),
            },
        };
        assert_eq!(progress_note(&started).as_deref(), Some("stage: plan"));
        let done = RunEvent {
            run_id: "run".into(),
            seq: 2,
            at: "2026-01-01T00:00:01Z".into(),
            kind: RunEventKind::StepCompleted {
                step: "plan".into(),
                outcome: "ok".into(),
                duration_ms: 1,
                counts: [("files".into(), 2)].into(),
            },
        };
        assert_eq!(progress_note(&done).as_deref(), Some("stage: plan files=2"));
        let other = RunEvent {
            run_id: "run".into(),
            seq: 3,
            at: "2026-01-01T00:00:02Z".into(),
            kind: RunEventKind::LifecycleChanged {
                from: RunLifecycle::ReviewingFiles,
                to: RunLifecycle::Completed,
            },
        };
        assert!(progress_note(&other).is_none());
    }

    #[test]
    fn cause_and_impact_follow_the_stored_body() {
        let (cause, impact) = split_body("the cause\n\nTrigger: the trigger\n\nImpact: the impact");
        assert_eq!(cause, "the cause");
        assert_eq!(impact, "the impact");
        let (cause, impact) = split_body("plain body");
        assert_eq!(cause, "plain body");
        assert!(impact.is_empty());
    }
}
