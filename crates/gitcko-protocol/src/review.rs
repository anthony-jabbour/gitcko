// SPDX-License-Identifier: Apache-2.0

//! `gitcko.review/1`: the result of one review, as `gitcko review --json` prints it and
//! local history stores it. Field names follow CLI_SPEC §6; Greptile's comment fields
//! keep the same meaning in snake_case.
//!
//! Readers accept fields they don't know, because adding a field doesn't bump the
//! version (CLI_SPEC §6). Removing or changing one does, to `gitcko.review/2`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::RunEvent;

/// The only version this crate reads or writes. Any other value fails to parse with a
/// message naming the supported one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ReviewSchemaVersion {
    #[serde(rename = "gitcko.review/1")]
    V1,
}

/// One review run and everything it found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Review {
    pub schema_version: ReviewSchemaVersion,
    /// `rv_` followed by a 26-character ULID-format id.
    pub review_id: String,
    pub status: ReviewStatus,
    /// Machine-readable reasons for a non-complete status, e.g. `config_invalid`.
    pub reason_codes: Vec<String>,
    pub tier: Tier,
    pub target: Target,
    pub summary: Option<String>,
    /// The run's free-form instructions, echoed so a result explains itself.
    pub instructions: Option<String>,
    /// Readiness score 0–5 (SCORING). `null` until the scoring gate passes, and always
    /// `null` when `status` isn't `complete`.
    #[schemars(range(max = 5))]
    pub score: Option<u8>,
    pub score_reasoning: Option<String>,
    pub score_details: Option<ScoreDetails>,
    pub score_availability_reason: Option<ScoreAvailabilityReason>,
    pub risk_level: Option<RiskAssessment>,
    pub security_summary: Option<String>,
    pub cleared_risks: Vec<ClearedRisk>,
    pub file_summaries: Vec<FileSummary>,
    /// Empty for a clean review, never `null`.
    pub findings: Vec<Finding>,
    /// Questions are not findings: they carry no severity and never count toward the
    /// score, `fail_on` or precision (REVIEW_RULES §8). At most two per review.
    pub questions: Vec<Question>,
    pub coverage: Coverage,
    pub versions: Versions,
    pub usage: Usage,
    /// The run's events, included only when asked for (`review show --events`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<RunEvent>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Complete,
    Partial,
    Failed,
    Canceled,
    Superseded,
}

/// Review depth. Local and Self-Host run `base` only (ADR-79).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Base,
    Plus,
    Max,
    Auto,
}

/// The immutable commits the review is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Target {
    pub base_sha: String,
    pub head_sha: String,
    pub merge_base: Option<String>,
    /// True when staged or unstaged changes were reviewed on top of `head_sha`.
    pub local_changes: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScoreDetails {
    /// Versioned scoring policy, e.g. `gitcko-score@1`.
    pub policy: String,
    pub source: ScoreSource,
    #[schemars(range(max = 5))]
    pub ceiling: u8,
    /// The ceiling rule that decided `ceiling`.
    pub ceiling_rule: String,
    pub matched_rules: Vec<String>,
    #[schemars(range(max = 5))]
    pub low_bound: u8,
    #[schemars(range(max = 5))]
    pub model_score: Option<u8>,
    /// Each point the scorer took off below the ceiling, with its evidence.
    pub adjustments: Vec<ScoreAdjustment>,
    /// Findings counted for scoring after the medium-confidence drop (SCORING §3.1).
    pub effective_counts: SeverityCounts,
    /// Share of changed files actually investigated, 0–1.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub coverage: f64,
}

/// Who picked the score: the scorer model, or the deterministic ceiling when the
/// model call failed (SCORING §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScoreSource {
    Model,
    Ceiling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScoreAdjustment {
    pub code: String,
    /// `path:line`, a rule id or a summary reference.
    pub evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SeverityCounts {
    #[serde(rename = "P0")]
    pub p0: u32,
    #[serde(rename = "P1")]
    pub p1: u32,
    #[serde(rename = "P2")]
    pub p2: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScoreAvailabilityReason {
    GateClosed,
    Incomplete,
    Failed,
    Canceled,
    Superseded,
    InputsMissing,
}

/// What the change does, separate from the score; only auto-approve uses it (ADR-43).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RiskAssessment {
    pub level: RiskLevel,
    pub source: RiskSource,
    pub reason: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// `rules` when the risk-classifier failed and deterministic rules decided (SCORING §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RiskSource {
    Model,
    Rules,
}

/// A risk the reviewer checked and found handled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClearedRisk {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileSummary {
    pub path: String,
    pub summary: String,
}

/// A concrete defect the change introduces or worsens (REVIEW_RULES §1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    /// `f_` followed by 16 hex characters: a stable fingerprint of the issue, not of
    /// its line numbers (REVIEW_ENGINE §16).
    pub id: String,
    /// Repository-relative, `/`-separated.
    pub path: String,
    #[schemars(range(min = 1))]
    pub start_line: u32,
    #[schemars(range(min = 1))]
    pub end_line: u32,
    pub side: Side,
    pub severity: Severity,
    pub comment_type: CommentType,
    pub security_issue: bool,
    /// The repository rule that triggered this finding, if any.
    pub linked_rule: Option<String>,
    /// Final confidence 0–1. Bands (High ≥ 0.85, Medium ≥ 0.70) are derived from it,
    /// never stored (REVIEW_ENGINE §15).
    #[schemars(range(min = 0.0, max = 1.0))]
    pub confidence: f64,
    /// `uncalibrated` means `confidence` is the model's assessment, not a measured
    /// probability.
    pub calibration_status: CalibrationStatus,
    pub title: String,
    pub body: String,
    pub evidence: Vec<Evidence>,
    pub verification: Verification,
    /// Typed when Verify lands; `null` until then.
    pub verified_evidence: Option<serde_json::Value>,
    pub suggestion: Option<String>,
    pub hunk: Option<Hunk>,
    /// Set when the finding sits outside the changed lines: the changed line it traces
    /// back to (REVIEW_RULES §9).
    pub outside_diff: Option<OutsideDiff>,
    pub lifecycle: Lifecycle,
    pub fix_package: Option<FixPackage>,
}

/// Which side of the diff a line number refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Old,
    New,
}

/// Impact times realistic reachability, never confidence. There is no P3 (ADR-13).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub enum Severity {
    P0,
    P1,
    P2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommentType {
    Logic,
    Syntax,
    Style,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationStatus {
    Calibrated,
    Uncalibrated,
}

/// A source location that supports a finding. Model prose alone is never evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub path: String,
    #[schemars(range(min = 1))]
    pub start_line: u32,
    #[schemars(range(min = 1))]
    pub end_line: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Source,
    Rule,
    Graph,
    Test,
}

/// Runtime verification outcome (ARCHITECTURE §7). Base reviews are always `not_run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    NotRun,
    Verified,
    NotReproduced,
    Inconclusive,
    Error,
}

/// The diff hunk a finding points into, with the lines before and after the fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hunk {
    pub header: String,
    pub old_range: LineRange,
    pub new_range: LineRange,
    pub before: String,
    pub after: String,
}

/// A hunk range as in `@@ -start,lines +start,lines @@`. `start` is 0 for an empty side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LineRange {
    pub start: u32,
    pub lines: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutsideDiff {
    pub linked_path: String,
    #[schemars(range(min = 1))]
    pub linked_line: u32,
}

/// Where the issue stands across reviews of the same PR (REVIEW_ENGINE §16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    New,
    StillPresent,
    PossiblyFixed,
    Resolved,
}

/// The hand-off a coding agent uses to fix the finding. Phase 4 adds its fields; adding
/// them is not a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FixPackage {
    pub schema_version: FixSchemaVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum FixSchemaVersion {
    #[serde(rename = "gitcko.fix/1")]
    V1,
}

/// A possible P0/P1 that needs one fact the repository can't answer (REVIEW_RULES §8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Question {
    /// `q_` followed by 16 hex characters.
    pub id: String,
    pub path: String,
    #[schemars(range(min = 1))]
    pub start_line: u32,
    #[schemars(range(min = 1))]
    pub end_line: u32,
    pub side: Side,
    pub body: String,
    /// What answer would make it a bug, and why that matters.
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Coverage {
    pub changed_files: u32,
    pub investigated_files: u32,
    /// Changed files the review didn't investigate, so a reader never mistakes
    /// partial coverage for a clean result.
    pub held_back: Vec<String>,
    pub skipped_binary: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Versions {
    pub engine: String,
    pub policy: String,
    pub routing_policy: String,
    pub index: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    pub model_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `null` when the provider doesn't report cost; never estimated as 0.
    pub cost_usd: Option<f64>,
}
