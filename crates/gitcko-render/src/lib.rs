// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plain-text rendering of a stored `gitcko.review/1`. CLI `--text`/`--agent`, TUI
//! parity, and MCP result text all go through here so an incomplete review can never
//! look like a clean one.

use gitcko_protocol::event::RunEvent;
use gitcko_protocol::review::{Finding, Review, ReviewStatus, Severity};

/// First line of every review rendering. Non-complete statuses are never just a score.
pub fn status_line(review: &Review) -> String {
    let name = match review.status {
        ReviewStatus::Complete => "complete",
        ReviewStatus::Partial => "partial",
        ReviewStatus::Failed => "failed",
        ReviewStatus::Canceled => "canceled",
        ReviewStatus::Superseded => "superseded",
    };
    if review.reason_codes.is_empty() {
        format!("status: {name}")
    } else {
        format!("status: {name} ({})", review.reason_codes.join(", "))
    }
}

/// `--text` / `--agent` body. The status line is always first. "No issues found" is
/// printed only for a complete review with no findings.
pub fn text(review: &Review) -> String {
    let mut out = String::new();
    out.push_str(&status_line(review));
    out.push('\n');
    if review.status == ReviewStatus::Complete {
        if review.findings.is_empty() {
            out.push_str("No issues found\n");
        } else if let Some(summary) = &review.summary {
            out.push_str(summary);
            out.push('\n');
        }
    } else if review.findings.is_empty() {
        out.push_str("Review incomplete: nothing was concluded about the changed paths.\n");
    } else if let Some(summary) = &review.summary {
        out.push_str(summary);
        out.push('\n');
    }
    for finding in &review.findings {
        out.push('\n');
        out.push_str(&finding_card(finding));
    }
    out
}

/// Same content as [`text`], wrapped in a fixed-width box for a TTY.
pub fn boxed(review: &Review, width: u16) -> String {
    let inner = text(review);
    let width = usize::from(width.clamp(40, 240));
    let bar = "-".repeat(width.saturating_sub(2));
    let mut out = format!("+{bar}+\n");
    for line in inner.lines() {
        let mut row = line
            .chars()
            .take(width.saturating_sub(4))
            .collect::<String>();
        while row.chars().count() < width.saturating_sub(4) {
            row.push(' ');
        }
        out.push_str(&format!("| {row} |\n"));
    }
    out.push_str(&format!("+{bar}+\n"));
    out
}

/// One finding as a comment card.
pub fn finding_card(finding: &Finding) -> String {
    format!(
        "[{}] {}\n{}:{}\n{}\n",
        severity_label(finding.severity),
        finding.title,
        finding.path,
        finding.start_line,
        finding.body
    )
}

/// Newest-first list for `review show` with no id.
pub fn list(items: &[ListItem]) -> String {
    let mut out = String::new();
    for item in items {
        out.push_str(&format!(
            "{}  {}  {}\n",
            item.review_id,
            status_name(item.status),
            short_sha(&item.head_sha)
        ));
    }
    out
}

/// `review show --events` timeline. Metadata only (OBSERVABILITY §4).
pub fn events(events: &[RunEvent]) -> String {
    let mut out = String::new();
    for event in events {
        let ty = event_type(event);
        out.push_str(&format!("{}  {}  {ty}\n", event.seq, event.at));
    }
    out
}

/// One row in a review list. The text form is what `review show` prints when no id
/// is given and stdout is not JSON, so a partial row can never be mistaken for a
/// clean one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    /// Stored id (`rv_` plus the ULID). Shown in full so a person can pass it to
    /// `review show`.
    pub review_id: String,
    /// End state of that run. Printed by name, never omitted.
    pub status: ReviewStatus,
    /// Full head commit. The list prints seven characters; the rest stays here so
    /// callers don't re-truncate a short id.
    pub head_sha: String,
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::P0 => "P0",
        Severity::P1 => "P1",
        Severity::P2 => "P2",
    }
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

fn short_sha(sha: &str) -> &str {
    let end = sha.char_indices().nth(7).map_or(sha.len(), |(i, _)| i);
    &sha[..end]
}

fn event_type(event: &RunEvent) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|v| v.get("type")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}
