// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the model sends back, parsed into closed types and then checked against bounds
//! serde can't express. Model output is hostile input (TM-06): anything outside these
//! types is refused with value-free problem codes, which the repair prompt lists by
//! field name so the model can correct itself once (MODEL_ROUTING §7).

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::Deserialize;

/// Longest reply parsed at all, in bytes; a larger one is refused unread.
pub const MAX_REPLY: usize = 256 * 1024;

/// The seven baseline categories (REVIEW_ENGINE §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Correctness,
    Security,
    DataIntegrity,
    Concurrency,
    Performance,
    ResourceManagement,
    RepositoryRule,
}

/// Severity as the model proposes it; the engine may lower it (REVIEW_RULES §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema)]
pub enum Severity {
    P0,
    P1,
    P2,
}

/// REVIEW_RULES §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommentType {
    Logic,
    Syntax,
    Style,
}

/// Which side of the diff a line is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SideOut {
    Old,
    New,
}

/// `risk`: how risky the change is, and which files to look at first.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RiskOutput {
    pub level: RiskLevel,
    /// One short sentence.
    pub reason: String,
    /// Changed files worth investigating, most important first.
    pub priorities: Vec<String>,
}

/// The change's overall risk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// `plan`: what might be wrong, as hypotheses to investigate.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanOutput {
    pub hypotheses: Vec<Hypothesis>,
}

/// One possible defect, before any evidence is gathered.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hypothesis {
    pub title: String,
    pub category: Category,
    /// The changed file it is about.
    pub path: String,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    /// What would be wrong, and what would show it.
    pub claim: String,
}

/// How an investigation ended (REVIEW_ENGINE §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Supported,
    Refuted,
    Inconclusive,
    Blocked,
}

/// `investigate`'s last message: the outcome and, when supported, the findings.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvestigationOutput {
    pub outcome: Outcome,
    /// Why, in one or two sentences.
    pub reason: String,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
}

/// A candidate finding (REVIEW_ENGINE §12). Not published until it passes validation and
/// the judge.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub title: String,
    pub category: Category,
    pub severity: Severity,
    pub comment_type: CommentType,
    pub confidence: f64,
    pub path: String,
    pub side: SideOut,
    pub start_line: u32,
    pub end_line: u32,
    /// What triggers it.
    pub trigger: String,
    /// The causal chain from the change to the failure.
    pub cause: String,
    /// What breaks, for whom.
    pub impact: String,
    /// Evidence ids (`e3`) from tool results that show it.
    pub evidence: Vec<String>,
    /// Evidence ids that argue against it, if any were found.
    #[serde(default)]
    pub counterevidence: Vec<String>,
    /// A safe fix, only if one is clear.
    pub suggestion: Option<String>,
}

/// `judge`: an independent decision on one candidate (REVIEW_ENGINE §14).
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JudgeOutput {
    pub verdict: Verdict,
    pub confidence: f64,
    /// A lower severity if the evidence supports only that; never higher.
    pub severity: Option<Severity>,
    pub reason: String,
    /// For `needs_follow_up`: the one fact outside the repository that would settle it.
    pub missing_fact: Option<String>,
}

/// The judge's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Accept,
    Reject,
    NeedsFollowUp,
}

/// What was wrong with a reply. Codes and our own field names only: no model text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum Problem {
    #[error("the reply is too large")]
    TooLarge,
    #[error("the reply is not one JSON object")]
    NotJson,
    #[error("the reply does not match the schema")]
    Schema,
    #[error("{0} is empty")]
    Empty(&'static str),
    #[error("{0} is too long")]
    TooLong(&'static str),
    #[error("{0} has too many items")]
    TooMany(&'static str),
    #[error("{0} must be a number from 0 to 1")]
    BadNumber(&'static str),
    #[error("{0}: line numbers start at 1 and end_line can't be before start_line")]
    BadLines(&'static str),
    #[error("{0} must be evidence ids like e3")]
    BadEvidence(&'static str),
    #[error("{0} is required for this outcome")]
    Missing(&'static str),
}

/// A reply that was refused, with every problem found (at most one per kind and field).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("model output refused ({} problems)", problems.len())]
pub struct OutputError {
    pub problems: Vec<Problem>,
}

/// Parses a reply as `T` and checks its bounds. Accepts the JSON object alone, or inside
/// one Markdown code fence, which many models add; anything else around it is refused.
pub fn parse<T: Checked>(reply: &str) -> Result<T, OutputError> {
    let fail = |p: Problem| OutputError { problems: vec![p] };
    if reply.len() > MAX_REPLY {
        return Err(fail(Problem::TooLarge));
    }
    let text = unfence(reply.trim());
    if !(text.starts_with('{') && text.ends_with('}')) {
        return Err(fail(Problem::NotJson));
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| fail(Problem::NotJson))?;
    let parsed: T = serde_json::from_value(value).map_err(|_| fail(Problem::Schema))?;
    let mut problems = Vec::new();
    parsed.check(&mut problems);
    let mut seen = HashSet::new();
    problems.retain(|p| seen.insert(*p));
    if problems.is_empty() {
        Ok(parsed)
    } else {
        Err(OutputError { problems })
    }
}

/// The inside of a single ```` ``` ```` or ```` ```json ```` block, or the text as is.
fn unfence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let Some(body) = rest.strip_suffix("```") else {
        return text;
    };
    // A second fence inside fails to parse as one JSON object anyway.
    body.strip_prefix("json").unwrap_or(body).trim()
}

/// Bounds a parsed reply must meet beyond its schema.
pub trait Checked: serde::de::DeserializeOwned + JsonSchema {
    /// Pushes every problem found.
    fn check(&self, problems: &mut Vec<Problem>);
}

fn text(problems: &mut Vec<Problem>, field: &'static str, value: &str, max: usize) {
    if value.trim().is_empty() {
        problems.push(Problem::Empty(field));
    } else if value.chars().count() > max {
        problems.push(Problem::TooLong(field));
    }
}

fn count<T>(problems: &mut Vec<Problem>, field: &'static str, items: &[T], max: usize) {
    if items.len() > max {
        problems.push(Problem::TooMany(field));
    }
}

fn probability(problems: &mut Vec<Problem>, field: &'static str, value: f64) {
    // Written so NaN fails too.
    if !(0.0..=1.0).contains(&value) {
        problems.push(Problem::BadNumber(field));
    }
}

fn evidence(problems: &mut Vec<Problem>, field: &'static str, ids: &[String]) {
    let valid = |id: &String| {
        id.strip_prefix('e')
            .is_some_and(|n| !n.is_empty() && n.len() <= 6 && n.bytes().all(|b| b.is_ascii_digit()))
    };
    if !ids.iter().all(valid) {
        problems.push(Problem::BadEvidence(field));
    }
}

fn path(problems: &mut Vec<Problem>, field: &'static str, value: &str) {
    if gitcko_domain::RepoPath::parse(value).is_err() {
        problems.push(Problem::Schema);
    }
    text(problems, field, value, 400);
}

impl Checked for RiskOutput {
    fn check(&self, p: &mut Vec<Problem>) {
        text(p, "reason", &self.reason, 300);
        count(p, "priorities", &self.priorities, 50);
        for f in &self.priorities {
            path(p, "priorities", f);
        }
    }
}

impl Checked for PlanOutput {
    fn check(&self, p: &mut Vec<Problem>) {
        count(p, "hypotheses", &self.hypotheses, 8);
        for h in &self.hypotheses {
            text(p, "title", &h.title, 200);
            text(p, "claim", &h.claim, 1000);
            path(p, "path", &h.path);
            match (h.start_line, h.end_line) {
                (Some(s), Some(e)) if s == 0 || e < s => p.push(Problem::BadLines("start_line")),
                (Some(0), None) | (None, Some(0)) => p.push(Problem::BadLines("start_line")),
                _ => {}
            }
        }
    }
}

impl Checked for InvestigationOutput {
    fn check(&self, p: &mut Vec<Problem>) {
        text(p, "reason", &self.reason, 600);
        count(p, "candidates", &self.candidates, 3);
        if self.outcome == Outcome::Supported && self.candidates.is_empty() {
            p.push(Problem::Missing("candidates"));
        }
        for c in &self.candidates {
            text(p, "title", &c.title, 200);
            text(p, "trigger", &c.trigger, 1000);
            text(p, "cause", &c.cause, 2000);
            text(p, "impact", &c.impact, 1000);
            if let Some(s) = &c.suggestion {
                text(p, "suggestion", s, 2000);
            }
            path(p, "path", &c.path);
            probability(p, "confidence", c.confidence);
            if c.start_line == 0 || c.end_line < c.start_line {
                p.push(Problem::BadLines("start_line"));
            }
            if c.evidence.is_empty() {
                p.push(Problem::Empty("evidence"));
            }
            count(p, "evidence", &c.evidence, 20);
            count(p, "counterevidence", &c.counterevidence, 20);
            evidence(p, "evidence", &c.evidence);
            evidence(p, "counterevidence", &c.counterevidence);
        }
    }
}

impl Checked for JudgeOutput {
    fn check(&self, p: &mut Vec<Problem>) {
        text(p, "reason", &self.reason, 600);
        probability(p, "confidence", self.confidence);
        match (&self.verdict, &self.missing_fact) {
            (Verdict::NeedsFollowUp, None) => p.push(Problem::Missing("missing_fact")),
            (_, Some(f)) => text(p, "missing_fact", f, 300),
            _ => {}
        }
    }
}
