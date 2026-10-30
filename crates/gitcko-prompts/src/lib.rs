// SPDX-License-Identifier: AGPL-3.0-or-later

//! Gitcko's review prompts (REVIEW_ENGINE §10–14), versioned, and the strict parsers for
//! what comes back.
//!
//! - **Prompts** are fixed policy text plus fenced repository input, as model
//!   [`Request`](gitcko_model::Request)s. Each has an `id@version`; editing its text
//!   without bumping the version fails a test, and a bump changes every recorded replay
//!   keyed on it (DEVELOPMENT §6).
//! - **The fence** ([`fence`]) wraps repository text in markers derived from its hash, so
//!   it can't close its own block (SECURITY §17.3).
//! - **Outputs** ([`parse`]) are closed types with bounds; a refused reply gets value-free
//!   [`Problem`] codes, which [`repair`] turns into the single repair attempt.

mod fence;
mod output;
mod render;

pub use fence::fence;
pub use output::{
    Candidate, Category, Checked, CommentType, Hypothesis, InvestigationOutput, JudgeOutput,
    MAX_REPLY, Outcome, OutputError, PlanOutput, Problem, RiskLevel, RiskOutput, Severity, SideOut,
    Verdict, parse,
};
pub use render::{
    BUNDLE_VERSION, ChangeContext, INVESTIGATE, JUDGE, PLAN, REPAIR, RISK, fixed_texts,
    investigate, judge, plan, repair, risk, tool_result,
};
