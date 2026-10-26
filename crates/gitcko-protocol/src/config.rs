// SPDX-License-Identifier: Apache-2.0

//! The shape of one config file, before any merging: `.gitcko/config.toml` (a repo
//! folder) and `~/.config/gitcko/config.toml` (the user). Every key is optional so the
//! merge (`gitcko-config`, CONFIGURATION §3) can tell "unset" from a value.
//!
//! Unknown keys are rejected rather than ignored (CONFIGURATION §11), which also stops
//! a repository file from setting models, credentials or network policy (§2). Range and
//! cross-field checks with file and line numbers happen in `gitcko-config`.

use std::collections::BTreeMap;

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
    /// Model bindings, roles and fallbacks (MODEL_ROUTING). Keys never live here; they
    /// come from the keychain or environment.
    pub models: Option<ModelsConfig>,
}

/// `[models]` (MODEL_ROUTING §3). Typed and closed, so a key field or a misspelt
/// option is an error instead of being silently ignored. Where a request goes
/// (on device or remote) is computed from the binding, never set here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    /// Named bindings. Names are `[a-z0-9_-]`, so dotted config keys stay unambiguous.
    pub bindings: Option<BTreeMap<String, ModelBinding>>,
    /// Role → binding name; unset roles use their class default.
    pub roles: Option<BTreeMap<ModelRole, String>>,
    /// Class → binding names tried in order after the primary fails.
    pub fallbacks: Option<BTreeMap<ModelClass, Vec<String>>>,
    pub inversion: Option<Inversion>,
}

/// One provider and exact model. A moving alias (`latest`, an untagged Ollama name)
/// is rejected in `gitcko-config`, because a review must name the model it ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelBinding {
    pub provider: ModelProvider,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Overrides the family taken from the OpenRouter vendor prefix (`open-weight`
    /// for Ollama); the router uses it to keep fallbacks in a different family.
    pub family: Option<ModelFamily>,
    /// OpenRouter only. Unset means `deny_collection`.
    pub data_policy: Option<DataPolicy>,
    /// Ollama only. Unset means `OLLAMA_HOST`, else `http://127.0.0.1:11434`.
    pub endpoint: Option<String>,
    /// Ollama only: sent as `num_ctx`, since Ollama's own default is too small for
    /// review prompts. Unset means the class floor.
    #[schemars(range(min = 2048, max = 1048576))]
    pub context: Option<u32>,
}

/// The providers Phase 3 ships adapters for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelProvider {
    Openrouter,
    Ollama,
}

/// How hard a reasoning model thinks, where the provider supports it; unset means
/// the provider's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
}

/// Model families, for keeping a fallback or a judge out of the primary's family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ModelFamily {
    Anthropic,
    Openai,
    Google,
    OpenWeight,
}

/// What OpenRouter may let the upstream provider do with prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DataPolicy {
    /// Only providers that don't store or train on prompts (`data_collection: deny`).
    DenyCollection,
    /// Only zero-data-retention endpoints.
    Zdr,
    /// The account's OpenRouter setting.
    ProviderDefault,
}

/// The stable role ids of MODEL_ROUTING §2. Renaming one is a breaking change.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ModelRole {
    RiskClassifier,
    ReviewLight,
    Review,
    ReviewDeep,
    Judge,
    Refiner,
    Summary,
    Scorer,
    Verify,
    Ask,
    Embeddings,
    ReplyAgent,
    ReReviewGate,
    AddressedJudge,
    MemoryLearner,
    Digest,
}

/// Capability classes roles resolve to (MODEL_ROUTING §3).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ModelClass {
    Fast,
    Smart,
    Deep,
    Embed,
}

/// Model inversion (MODEL_ROUTING §9): review with a model from a different family
/// than the one that wrote the PR. On by default; a no-op with one family.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inversion {
    pub enabled: Option<bool>,
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
