// SPDX-License-Identifier: AGPL-3.0-or-later

//! The review engine (REVIEW_ENGINE §4). [`review`] takes a commit range and returns a
//! typed `gitcko.review/1`: snapshot, a disposition for every changed path, changed
//! symbols and risk signals, risk classification, hypotheses, then one bounded
//! investigation per hypothesis.
//!
//! Three things hold everywhere. Every model call goes through [`gitcko_model::Router`],
//! so one budget, one deadline and one cancel cover all of them, and locality is the
//! router's to enforce. Repository text and model output are data: the first stays in
//! the prompt fence, the second is parsed into closed types. And a result is `complete`
//! only when nothing was left out; every gap is a reason code ([`complete`]).
//!
//! This is the first half. Candidates are published as findings after only an evidence
//! check, so **no surface may ship on this** until validation, the judge and the gates
//! land (Phase 4, batch B4b). The score stays `null` regardless.

mod complete;
mod events;
mod investigate;
mod plan;
mod providers;
mod result;
mod risk;
mod snapshot;
mod symbols;

use gitcko_git::{CommitId, Repo};
use gitcko_model::{BudgetLimits, CallRecord, Router, Run, RunBudget};
use gitcko_protocol::config::ModelRole;
use gitcko_protocol::event::RunEvent;
use gitcko_protocol::review::Review;
use std::time::Duration;

use tokio::time::Instant;

pub use complete::{Completeness, reason};
pub use providers::{
    FeedbackProvider, GraphSignals, Knowledge, KnowledgeProvider, NoFeedback, ProviderStatus,
    RepoKnowledge, RiskSignalProvider, Signal,
};
pub use snapshot::{Disposition, FileDisposition};

/// Base's bounds (REVIEW_ENGINE §9). Versioned as `review_policy@2`, recorded in the
/// result, and not user config.
pub const POLICY: &str = "review_policy@2";
/// Hypotheses one review investigates.
pub const MAX_HYPOTHESES: usize = 8;
/// Tool calls one hypothesis may run; then the model answers from what it has.
pub const ACTIONS_PER_HYPOTHESIS: u32 = 12;
/// Tool calls one review may run.
pub const ACTIONS_PER_RUN: u32 = 80;
/// Turns in a row with no new tool result before an investigation is called inconclusive.
pub const NO_PROGRESS_TURNS: u32 = 3;
/// Files one review reads the diff of; past this the rest are `truncated`.
pub const MAX_FILES: usize = 100;
/// Diff lines shown to the model, per file and in all.
pub const MAX_FILE_DIFF_LINES: usize = 1500;
pub const MAX_DIFF_LINES: usize = 4000;
/// Set aside for validation, the judge and assembly: investigation stops here.
pub const RESERVE_PERCENT: u64 = 25;

/// How long a remote review may take. Local runs get 15 minutes when the CLI brings them.
pub const REMOTE_DEADLINE: Duration = Duration::from_secs(5 * 60);

/// The token ceilings for one Base review.
pub fn base_budget() -> BudgetLimits {
    BudgetLimits {
        input_tokens: 400_000,
        output_tokens: 40_000,
        cost_usd: None,
    }
}

/// A fresh run under Base's bounds. Only `review` is required until the judge lands; a
/// remote run gets the deadline, and a replay runs without one so it never races a clock.
pub fn base_run(id: String, remote: bool) -> Run {
    let mut run = Run::new(id, RunBudget::new(base_budget()));
    run.required = vec![ModelRole::Review];
    if remote {
        run.deadline = Some(Instant::now() + REMOTE_DEADLINE);
    }
    run
}

/// What to review: `base..head` of a committed comparison. Staged and working-copy
/// snapshots arrive with the CLI's flags.
pub struct ReviewInput<'a> {
    pub repo: &'a Repo,
    pub base: CommitId,
    pub head: CommitId,
    /// RFC 3339 time for events, passed in so results don't depend on the clock.
    pub now: &'a dyn Fn() -> String,
}

/// A finished run: the result, plus what each model attempt and step did.
pub struct Outcome {
    pub review: Review,
    pub records: Vec<CallRecord>,
    pub events: Vec<RunEvent>,
}

/// Runs the review. Never panics on repository content or model output; every failure
/// is in the returned result's status and reason codes.
pub async fn review(input: ReviewInput<'_>, router: &Router, mut run: Run) -> Outcome {
    let mut events = events::Events::new(&run.id, input.now);
    let mut ledger = complete::Completeness::default();
    // Reserve a share of the deadline; the token share is checked against spend.
    let investigate_until = run.deadline.map(|deadline| {
        let now = Instant::now();
        now + (deadline.saturating_duration_since(now) * (100 - RESERVE_PERCENT as u32)) / 100
    });

    events.started("snapshot");
    let snap = match snapshot::take(&input) {
        Ok(snap) => snap,
        Err(reason_code) => {
            ledger.fail(reason_code);
            events.completed("snapshot", "failed", &[]);
            return result::failed(&input, &run, ledger, events);
        }
    };
    events.completed(
        "snapshot",
        "ok",
        &[("paths", snap.dispositions.len() as u64)],
    );

    events.started("dispositions");
    snap.report_gaps(&mut ledger);
    let investigated = snap.investigated();
    events.completed(
        "dispositions",
        "ok",
        &[("paths", investigated.len() as u64)],
    );

    events.started("changed_symbols");
    let changed = symbols::changed(&snap);
    let graph_signals = GraphSignals {
        snapshot: &snap,
        changed: &changed,
    };
    let signals = providers::signals_or_empty(&graph_signals);
    events.completed(
        "changed_symbols",
        "ok",
        &[
            ("symbols", changed.len() as u64),
            ("signals", signals.len() as u64),
        ],
    );

    let knowledge = RepoKnowledge { snapshot: &snap };
    let context = snap.change_context(&knowledge, &signals);
    let mut calls = investigate::Calls::new(router, &mut run, &mut events);

    calls.events.started("risk");
    let risk = risk::classify(&mut calls, &context, &signals, &snap).await;
    calls.events.completed("risk", "ok", &[]);

    let mut investigation = investigate::Findings::default();
    if !ledger.stopped() {
        calls.events.started("plan");
        let hypotheses =
            plan::plan(&mut calls, &context, &signals, &risk, &snap, &mut ledger).await;
        calls
            .events
            .completed("plan", "ok", &[("hypotheses", hypotheses.len() as u64)]);
        if !ledger.stopped() {
            calls.events.started("investigate");
            investigation = investigate::run_all(
                &mut calls,
                &snap,
                hypotheses,
                investigate_until,
                &mut ledger,
            )
            .await;
            let counts = investigation.counts();
            calls.events.completed("investigate", "ok", &counts);
        }
    }

    let records = std::mem::take(&mut calls.records);
    drop(calls);
    result::assemble(
        &input,
        &run,
        &snap,
        risk,
        investigation,
        ledger,
        events,
        records,
    )
}
