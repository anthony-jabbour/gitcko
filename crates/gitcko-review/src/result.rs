// SPDX-License-Identifier: AGPL-3.0-or-later

//! The final `gitcko.review/1`. The score is `null` throughout Base: `gate_closed` when
//! complete, otherwise the reason it's incomplete, so a gap never reads as a number.

use gitcko_domain::ContentHash;
use gitcko_git::CommitId;
use gitcko_model::{CallRecord, POLICY as ROUTING_POLICY, Run};
use gitcko_prompts::{
    BUNDLE_VERSION, Candidate, Category, CommentType as ModelType, Severity as ModelSeverity,
    SideOut,
};
use gitcko_protocol::review::{
    CalibrationStatus, ClearedRisk, CommentType, Coverage, Evidence, EvidenceKind, Finding,
    Lifecycle, Review, ReviewSchemaVersion, ReviewStatus, RiskAssessment, RiskLevel, RiskSource,
    ScoreAvailabilityReason, Severity, Side, Target, Tier, Usage, Verification, Versions,
};
use gitcko_tools::Ledger;

use crate::complete::Completeness;
use crate::events::Events;
use crate::investigate::Findings;
use crate::plan::Task;
use crate::risk::Risk;
use crate::snapshot::{Disposition, Snapshot};
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

/// Stable under moved lines and reworded titles' case and spacing (REVIEW_ENGINE §16).
fn finding_id(path: &str, category: Category, title: &str) -> String {
    let words: Vec<String> = title.split_whitespace().map(str::to_lowercase).collect();
    let text = format!("{path}\u{0}{category:?}\u{0}{}", words.join(" "));
    let hash = ContentHash::of(text.as_bytes()).to_string();
    let hex = hash.rsplit(':').next().unwrap_or(&hash);
    format!("f_{}", &hex[..16])
}

fn finding(task: &Task, candidate: Candidate, ledger: &Ledger) -> Finding {
    let _ = task;
    let evidence = candidate
        .evidence
        .iter()
        .filter_map(|id| ledger.get(id))
        .map(|e| Evidence {
            kind: EvidenceKind::Source,
            path: e.path.as_str().to_string(),
            start_line: e.range.start(),
            end_line: e.range.end(),
        })
        .collect();
    Finding {
        id: finding_id(&candidate.path, candidate.category, &candidate.title),
        path: candidate.path,
        start_line: candidate.start_line,
        end_line: candidate.end_line,
        side: match candidate.side {
            SideOut::Old => Side::Old,
            SideOut::New => Side::New,
        },
        severity: match candidate.severity {
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
        linked_rule: None,
        confidence: candidate.confidence,
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
        outside_diff: None,
        lifecycle: Lifecycle::New,
        fix_package: None,
    }
}

fn summary(status: ReviewStatus, findings: usize, files: u32) -> String {
    let state = match status {
        ReviewStatus::Complete => "complete",
        ReviewStatus::Partial => "incomplete",
        ReviewStatus::Failed => "failed",
        ReviewStatus::Canceled => "canceled",
        ReviewStatus::Superseded => "superseded",
    };
    format!("Review {state}: {findings} finding(s) across {files} investigated file(s).")
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
        summary: Some(summary(status, 0, 0)),
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
    ledger: Completeness,
    events: Events<'_>,
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
    let mut findings: Vec<Finding> = found
        .candidates
        .into_iter()
        .map(|(task, candidate)| finding(&task, candidate, &found.ledger))
        .collect();
    findings.sort_by(|a, b| {
        (a.severity, a.path.as_str(), a.start_line, a.id.as_str()).cmp(&(
            b.severity,
            b.path.as_str(),
            b.start_line,
            b.id.as_str(),
        ))
    });
    let review = Review {
        schema_version: ReviewSchemaVersion::V1,
        review_id: run.id.clone(),
        status,
        reason_codes: ledger.reason_codes(),
        tier: Tier::Base,
        target: target(input.base, input.head, snapshot.merge_base),
        summary: Some(summary(status, findings.len(), investigated)),
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
        questions: Vec::new(),
        coverage: Coverage {
            changed_files: snapshot.dispositions.len() as u32,
            investigated_files: investigated,
            held_back: paths(Disposition::HeldBack),
            skipped_binary: paths(Disposition::Binary),
        },
        versions: versions(
            snapshot.config.policy_digest(),
            snapshot.index.versions.label(),
        ),
        usage: usage(&records),
        events: None,
    };
    Outcome {
        review,
        records,
        events: events.into_vec(),
    }
}
