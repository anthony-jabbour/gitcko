// SPDX-License-Identifier: Apache-2.0

//! The shape of one config file, before any merging: `.gitcko/config.toml` (a repo
//! folder) and `~/.config/gitcko/config.toml` (the user). Every key is optional so the
//! merge (`gitcko-config`, CONFIGURATION §3) can tell "unset" from a value.
//!
//! Unknown keys are rejected rather than ignored (CONFIGURATION §11), which also stops
//! a repository file from setting models, credentials or network policy (§2). Range and
//! cross-field checks with file and line numbers happen in `gitcko-config`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::review::{CommentType, RiskLevel, Severity, Tier};

/// `.gitcko/config.toml` at the repo root or in any subfolder (CONFIGURATION §5).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    /// Required in the root config only; must be `1`.
    pub schema_version: Option<u32>,
    /// 1 = most verbose, 3 = critical only (same direction as Greptile).
    #[schemars(range(min = 1, max = 3))]
    pub strictness: Option<u8>,
    pub comment_types: Option<Vec<CommentType>>,
    pub tier: Option<Tier>,
    #[schemars(length(max = 4000))]
    pub instructions: Option<String>,
    pub fail_on: Option<Severity>,

    pub auto_review: Option<Vec<AutoReviewTrigger>>,
    pub review_drafts: Option<bool>,
    pub skip_auto_review: Option<bool>,
    pub labels: Option<Vec<String>>,
    pub disabled_labels: Option<Vec<String>>,
    pub include_authors: Option<Vec<String>>,
    pub exclude_authors: Option<Vec<String>>,
    /// Globs over the PR's target branch.
    pub include_branches: Option<Vec<String>>,
    pub exclude_branches: Option<Vec<String>>,
    /// Matched against the PR title and body.
    pub include_keywords: Option<Vec<String>>,
    pub ignore_keywords: Option<Vec<String>>,
    /// gitignore-syntax patterns for files the review skips.
    pub ignore: Option<Vec<String>>,

    pub check_run: Option<bool>,
    pub summary_only: Option<bool>,
    pub fix_links: Option<bool>,
    pub hide_footer: Option<bool>,
    pub update_description: Option<bool>,

    pub auto_approve: Option<AutoApprove>,
    pub rules: Option<Vec<Rule>>,
    /// Ids of inherited rules to drop in this folder and below.
    pub disabled_rules: Option<Vec<String>>,
    pub context: Option<Context>,
    pub summary: Option<Summary>,
}

/// Each trigger implies the earlier ones: `push` also reviews on open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutoReviewTrigger {
    Open,
    Push,
    Rebase,
}

/// Always read from the base branch and merged strictest-wins (CONFIGURATION §5.4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AutoApprove {
    pub enabled: Option<bool>,
    pub risk_ceiling: Option<RiskLevel>,
    pub filters: Option<AutoApproveFilters>,
}

/// Paths and authors that auto-approve never applies to, however clean the review
/// (CONFIGURATION §5.4). Merged strictest-wins across configs, so a PR touching an
/// excluded path is never auto-approved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AutoApproveFilters {
    pub exclude_paths: Option<Vec<String>>,
    pub include_paths: Option<Vec<String>>,
    pub exclude_authors: Option<Vec<String>>,
}

/// A structured review rule. Rules can raise attention and severity floors but never
/// waive evidence or security gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Needed to disable the rule further down the tree.
    pub id: Option<String>,
    pub rule: String,
    /// Globs relative to the folder that holds this `.gitcko/`.
    pub scope: Option<Vec<String>>,
    /// Minimum severity when violated.
    pub severity: Option<Severity>,
    pub comment_type: Option<CommentType>,
    pub enabled: Option<bool>,
}

/// Background the reviewer reads besides the diff (CONFIGURATION §5.6). Context files
/// from every folder level are combined, not replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub files: Option<Vec<ContextFile>>,
    /// Read-only cross-repo context on the same host and installation (Later).
    pub repos: Option<Vec<String>>,
}

/// A repository file the reviewer reads for background, like a schema or API spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextFile {
    /// Relative to the folder that holds this `.gitcko/`; `../` is rejected.
    pub path: String,
    pub description: Option<String>,
    /// Include only when reviewing files that match.
    pub scope: Option<Vec<String>>,
}

/// Which sections the PR summary shows. Each table merges field by field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub overview: Option<SummarySection>,
    pub files_table: Option<SummarySection>,
    pub score: Option<SummarySection>,
    pub diagram: Option<SummarySection>,
    pub security: Option<SummarySection>,
}

/// Display settings for one section of the PR summary. Every field is optional, so a
/// nested config can change one setting and keep the rest (merged field by field).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SummarySection {
    pub included: Option<bool>,
    pub collapsible: Option<bool>,
    pub default_open: Option<bool>,
}

/// `~/.config/gitcko/config.toml` (CONFIGURATION §6). Local runs only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    pub color: Option<bool>,
    /// Off unless set to `true` (stricter than Greptile, which defaults on).
    pub telemetry: Option<bool>,
    /// HTTPS origin with no path; plain HTTP only for loopback.
    pub api_url: Option<String>,
    pub web_url: Option<String>,
    pub review: Option<UserReview>,
    /// Model bindings, roles and fallbacks (MODEL_ROUTING). An open table until the
    /// model layer types it in Phase 3. Keys never live here; they come from the
    /// keychain or environment.
    pub models: Option<serde_json::Map<String, serde_json::Value>>,
}

/// How `gitcko review` prints results on this machine. User settings only change
/// output, never what the review finds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserReview {
    pub output: Option<OutputFormat>,
    pub layout: Option<Layout>,
    /// Lines of context around each finding.
    #[schemars(range(max = 60))]
    pub context: Option<u8>,
    #[schemars(range(min = 40, max = 240))]
    pub width: Option<u16>,
}

/// `auto` shows the interactive view on a terminal and plain text when piped
/// (CLI_SPEC); `json` is always asked for explicitly, so its shape is a promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    Auto,
    Text,
    Json,
}

/// `comments` lists findings like review comments; `diff` shows them inline in the
/// diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Comments,
    Diff,
}
