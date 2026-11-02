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
//! A candidate becomes a finding only after deterministic validation against the
//! snapshot, deduplication, suppression, an independent judge and the publication gates
//! ([`validate`], [`dedup`], [`suppress`], [`judge`], [`gates`]). The score stays `null`.

mod complete;
mod dedup;
mod events;
mod gates;
mod identity;
mod investigate;
mod judge;
mod plan;
mod providers;
mod result;
mod risk;
mod snapshot;
mod suppress;
mod symbols;
mod validate;

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
pub use validate::Dropped;

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

/// A fresh run under Base's bounds. `review` and `judge` are required: a candidate the
/// judge never answered for is never published, so losing it must fail the run, not hide
/// it. A remote run gets the deadline, and a replay runs without one so it never races a
/// clock.
pub fn base_run(id: String, remote: bool) -> Run {
    let mut run = Run::new(id, RunBudget::new(base_budget()));
    run.required = vec![ModelRole::Review, ModelRole::Judge];
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

fn counts(events: &mut events::Events<'_>, step: &str, candidates: usize, dropped: usize) {
    events.completed(
        step,
        "ok",
        &[
            ("candidates", candidates as u64),
            ("dropped", dropped as u64),
        ],
    );
}

/// A finished run: the result, plus what each model attempt and step did.
pub struct Outcome {
    pub review: Review,
    /// Candidates that left the pipeline, in order, with the stage and why. Codes only.
    pub dropped: Vec<Dropped>,
    pub records: Vec<CallRecord>,
    pub events: Vec<RunEvent>,
}

/// Runs the review. Never panics on repository content or model output; every failure
/// is in the returned result's status and reason codes.
pub async fn review(input: ReviewInput<'_>, router: &Router, run: Run) -> Outcome {
    review_with(input, router, run, &NoFeedback).await
}

/// [`review`] with a feedback source for suppression (REVIEW_RULES §7).
pub async fn review_with(
    input: ReviewInput<'_>,
    router: &Router,
    mut run: Run,
    feedback: &dyn FeedbackProvider,
) -> Outcome {
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

    let (items, mut dropped) = {
        calls.events.started("validate");
        let found = std::mem::take(&mut investigation.candidates);
        let (items, dropped) = validate::run(&snap, &investigation.ledger, found);
        counts(calls.events, "validate", items.len(), dropped.len());
        (items, dropped)
    };

    calls.events.started("dedup");
    let before = items.len();
    // A never-suppressed report is as good as an allowed one: it survives suppression.
    let mut items = dedup::run(items, &|item| {
        suppress::type_allowed(&snap, item) || suppress::never_suppressed(item)
    });
    counts(calls.events, "dedup", items.len(), before - items.len());
    // Ids are fixed here, before anything is dropped: feedback names a finding by its id,
    // and a sibling dropped later mustn't change the id of one that stays.
    let ids = identity::assign(
        &items
            .iter()
            .map(|i| {
                (
                    i.candidate.category,
                    i.entity.as_str(),
                    i.code.as_str(),
                    i.candidate.title.as_str(),
                )
            })
            .collect::<Vec<_>>(),
    );
    for (item, id) in items.iter_mut().zip(ids) {
        item.id = id;
    }

    calls.events.started("suppress");
    let (items, gone) = suppress::run(items, &snap, feedback);
    counts(calls.events, "suppress", items.len(), gone.len());
    dropped.extend(gone);

    calls.events.started("adjudicate");
    let asked = items.len();
    // A stopped run asks nothing more: what is left stays unjudged, so unpublished.
    let (judged, gone) = if ledger.stopped() {
        (Vec::new(), judge::all_unjudged(&items))
    } else {
        judge::run(&mut calls, items, &investigation.ledger, &mut ledger).await
    };
    counts(calls.events, "adjudicate", asked, gone.len());
    dropped.extend(gone);

    calls.events.started("gates");
    let verbose = snap
        .investigated()
        .into_iter()
        .map(|path| gates::policy(&snap, path).strictness)
        .min()
        .unwrap_or(2);
    let gated = gates::run(judged, &|path| gates::policy(&snap, path), verbose);
    calls.events.completed(
        "gates",
        "ok",
        &[
            ("findings", gated.findings.len() as u64),
            ("questions", gated.questions.len() as u64),
            ("dropped", gated.dropped.len() as u64),
        ],
    );
    dropped.extend(gated.dropped);

    calls.events.started("finalize");
    let records = std::mem::take(&mut calls.records);
    drop(calls);
    result::assemble(
        &input,
        &run,
        &snap,
        risk,
        investigation,
        (gated.findings, gated.questions),
        dropped,
        ledger,
        events,
        records,
    )
}
