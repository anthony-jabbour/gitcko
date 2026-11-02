// SPDX-License-Identifier: AGPL-3.0-or-later

//! The final `gitcko.review/1`. The score is `null` throughout Base: `gate_closed` when
//! complete, otherwise the reason it's incomplete, so a gap never reads as a number.

use std::collections::HashSet;

use gitcko_git::CommitId;
use gitcko_model::{CallRecord, POLICY as ROUTING_POLICY, Run};
use gitcko_prompts::{
    BUNDLE_VERSION, Category, CommentType as ModelType, Severity as ModelSeverity, SideOut,
};
use gitcko_protocol::review::{
    CalibrationStatus, ClearedRisk, CommentType, Coverage, Evidence, EvidenceKind, Finding,
    Lifecycle, Question, Review, ReviewSchemaVersion, ReviewStatus, RiskAssessment, RiskLevel,
    RiskSource, ScoreAvailabilityReason, Severity, Side, Target, Tier, Usage, Verification,
    Versions,
};
use gitcko_tools::Ledger;

use crate::complete::Completeness;
use crate::events::Events;
use crate::gates::Final;
use crate::investigate::Findings;
use crate::risk::Risk;
use crate::snapshot::{Disposition, Snapshot};
use crate::validate::Dropped;
use crate::{Outcome, POLICY, ReviewInput};

fn availability(status: ReviewStatus) -> ScoreAvailabilityReason {
    match status {
        ReviewStatus::Complete => ScoreAvailabilityReason::GateClosed,
        ReviewStatus::Partial => ScoreAvailabilityReason::Incomplete,
        ReviewStatus::Failed => ScoreAvailabilityReason::Failed,
        ReviewStatus::Canceled => ScoreAvailabilityReason::Canceled,
        ReviewStatus::Superseded => ScoreAvailabilityReason::Superseded,
    }
}

fn usage(records: &[CallRecord]) -> Usage {
    let costs: Vec<f64> = records.iter().filter_map(|r| r.cost_usd).collect();
    Usage {
        model_calls: records.len() as u32,
        input_tokens: records.iter().filter_map(|r| r.input_tokens).sum(),
        output_tokens: records.iter().filter_map(|r| r.output_tokens).sum(),
        // Unknown stays unknown: one unreported call makes the total a guess.
        cost_usd: (costs.len() == records.len() && !costs.is_empty()).then(|| costs.iter().sum()),
    }
}

fn versions(digest: String, index: String) -> Versions {
    Versions {
        engine: env!("CARGO_PKG_VERSION").to_string(),
        policy: POLICY.to_string(),
        routing_policy: ROUTING_POLICY.to_string(),
        index,
        prompts: BUNDLE_VERSION.to_string(),
        policy_digest: digest,
    }
}

fn target(base: CommitId, head: CommitId, merge_base: Option<CommitId>) -> Target {
    Target {
        base_sha: base.to_string(),
        head_sha: head.to_string(),
        merge_base: merge_base.map(|m| m.to_string()),
        local_changes: false,
    }
}

fn finding(done: Final, ledger: &Ledger) -> Finding {
    let Final {
        item,
        id,
        severity,
        confidence,
    } = done;
    let candidate = item.candidate;
    // What the candidate cited, then the other places the same cause was reported.
    let mut evidence: Vec<Evidence> = candidate
        .evidence
        .iter()
        .filter_map(|id| ledger.get(id))
        .map(|e| Evidence {
            kind: EvidenceKind::Source,
            path: e.path.as_str().to_string(),
            start_line: e.range.start(),
            end_line: e.range.end(),
        })
        .chain(item.also)
        .collect();
    let mut seen = HashSet::new();
    evidence.retain(|e| seen.insert((e.path.clone(), e.start_line, e.end_line)));
    Finding {
        id,
        path: candidate.path,
        start_line: candidate.start_line,
        end_line: candidate.end_line,
        side: match candidate.side {
            SideOut::Old => Side::Old,
            SideOut::New => Side::New,
        },
        severity: match severity {
            ModelSeverity::P0 => Severity::P0,
            ModelSeverity::P1 => Severity::P1,
            ModelSeverity::P2 => Severity::P2,
        },
        comment_type: match candidate.comment_type {
            ModelType::Logic => CommentType::Logic,
            ModelType::Syntax => CommentType::Syntax,
            ModelType::Style => CommentType::Style,
        },
        security_issue: candidate.category == Category::Security,
        linked_rule: item.rule,
        confidence,
        calibration_status: CalibrationStatus::Uncalibrated,
        title: candidate.title,
        body: format!(
            "{}\n\nTrigger: {}\n\nImpact: {}",
            candidate.cause, candidate.trigger, candidate.impact
        ),
        evidence,
        verification: Verification::NotRun,
        verified_evidence: None,
        suggestion: candidate.suggestion,
        hunk: None,
        outside_diff: item.outside,
        lifecycle: Lifecycle::New,
        fix_package: None,
    }
}

fn summary(status: ReviewStatus, findings: usize, files: u32, unresolved: u64) -> String {
    let state = match status {
        ReviewStatus::Complete => "complete",
        ReviewStatus::Partial => "incomplete",
        ReviewStatus::Failed => "failed",
        ReviewStatus::Canceled => "canceled",
        ReviewStatus::Superseded => "superseded",
    };
    let text =
        format!("Review {state}: {findings} finding(s) across {files} investigated file(s).");
    match unresolved {
        0 => text,
        n => format!("{text} {n} hypothesis(es) could not be settled either way."),
    }
}

/// The result of a run that couldn't read the repository.
pub fn failed(
    input: &ReviewInput<'_>,
    run: &Run,
    ledger: Completeness,
    events: Events<'_>,
) -> Outcome {
    let status = ledger.status();
    let review = Review {
        schema_version: ReviewSchemaVersion::V1,
        review_id: run.id.clone(),
        status,
        reason_codes: ledger.reason_codes(),
        tier: Tier::Base,
        target: target(input.base, input.head, None),
        summary: Some(summary(status, 0, 0, 0)),
        instructions: None,
        score: None,
        score_reasoning: None,
        score_details: None,
        score_availability_reason: Some(availability(status)),
        risk_level: Some(RiskAssessment {
            level: RiskLevel::Low,
            source: RiskSource::Rules,
            reason: "the repository could not be read".into(),
        }),
        security_summary: None,
        cleared_risks: Vec::new(),
        file_summaries: Vec::new(),
        findings: Vec::new(),
        questions: Vec::new(),
        coverage: Coverage {
            changed_files: 0,
            investigated_files: 0,
            held_back: Vec::new(),
            skipped_binary: Vec::new(),
        },
        versions: versions(String::new(), String::new()),
        usage: usage(&[]),
        events: None,
    };
    Outcome {
        review,
        dropped: Vec::new(),
        records: Vec::new(),
        events: events.into_vec(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn assemble(
    input: &ReviewInput<'_>,
    run: &Run,
    snapshot: &Snapshot<'_>,
    risk: Risk,
    found: Findings,
    gated: (Vec<Final>, Vec<Question>),
    dropped: Vec<Dropped>,
    ledger: Completeness,
    mut events: Events<'_>,
    records: Vec<CallRecord>,
) -> Outcome {
    let status = ledger.status();
    let paths = |want: Disposition| {
        snapshot
            .dispositions
            .iter()
            .filter(|d| d.disposition == want)
            .map(|d| d.path.as_str().to_string())
            .collect::<Vec<_>>()
    };
    let investigated = snapshot.investigated().len() as u32;
    let (finals, questions) = gated;
    // Already in order: most severe, then most confident.
    let findings: Vec<Finding> = finals
        .into_iter()
        .map(|done| finding(done, &found.ledger))
        .collect();
    events.completed(
        "finalize",
        "ok",
        &[
            ("findings", findings.len() as u64),
            ("questions", questions.len() as u64),
        ],
    );
    let review = Review {
        schema_version: ReviewSchemaVersion::V1,
        review_id: run.id.clone(),
        status,
        reason_codes: ledger.reason_codes(),
        tier: Tier::Base,
        target: target(input.base, input.head, snapshot.merge_base),
        summary: Some(summary(
            status,
            findings.len(),
            investigated,
            found.inconclusive,
        )),
        instructions: None,
        score: None,
        score_reasoning: None,
        score_details: None,
        score_availability_reason: Some(availability(status)),
        risk_level: Some(risk.assessment),
        security_summary: None,
        cleared_risks: found
            .refuted
            .into_iter()
            .map(|(task, why)| ClearedRisk {
                name: task.title,
                description: why,
            })
            .collect(),
        file_summaries: Vec::new(),
        findings,
        questions,
        coverage: Coverage {
            changed_files: snapshot.dispositions.len() as u32,
            investigated_files: investigated,
            held_back: paths(Disposition::HeldBack),
            skipped_binary: paths(Disposition::Binary),
        },
        versions: versions(snapshot.policy_digest(), snapshot.index.versions.label()),
        usage: usage(&records),
        events: None,
    };
    Outcome {
        review,
        dropped,
        records,
        events: events.into_vec(),
    }
}
