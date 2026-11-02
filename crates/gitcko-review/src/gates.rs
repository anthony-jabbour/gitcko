// SPDX-License-Identifier: AGPL-3.0-or-later

//! Confidence, severity and publication gates (REVIEW_ENGINE §15, REVIEW_RULES §4–5).

use gitcko_domain::RepoPath;
use gitcko_prompts::{Category, CommentType as Kind, JudgeOutput, Severity, SideOut, Verdict};
use gitcko_protocol::review::{CommentType, Question, Side};

use crate::snapshot::Snapshot;
use crate::validate::{Dropped, Item};

/// Questions one review may ask (REVIEW_RULES §5.1).
const MAX_QUESTIONS: usize = 2;

/// The strictness and comment types that govern one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub strictness: u8,
    pub types: Vec<CommentType>,
}

/// What governs `path`: its own folder's config, the defaults if there is none.
pub fn policy(snapshot: &Snapshot<'_>, path: &RepoPath) -> Policy {
    let config = snapshot
        .configs
        .get(path)
        .map(|resolved| resolved.repo_config())
        .unwrap_or_default();
    Policy {
        strictness: config.strictness.unwrap_or(2),
        types: config
            .comment_types
            .unwrap_or_else(|| vec![CommentType::Logic, CommentType::Syntax, CommentType::Style]),
    }
}

/// A candidate with the judge's answer.
#[derive(Debug, Clone)]
pub struct Judged {
    pub item: Item,
    pub judge: JudgeOutput,
}

/// A finding that passed every gate, ready to be written out.
#[derive(Debug, Clone)]
pub struct Final {
    pub item: Item,
    pub id: String,
    pub severity: Severity,
    pub confidence: f64,
}

#[derive(Default)]
pub struct Gated {
    pub findings: Vec<Final>,
    pub questions: Vec<Question>,
    pub dropped: Vec<Dropped>,
}

const HIGH: f64 = 0.85;
const MEDIUM: f64 = 0.70;
/// Style at strictness 2 is on but rare (REVIEW_RULES §4).
const STYLE_BAR: f64 = 0.92;

fn drop(out: &mut Gated, reason: &'static str) {
    out.dropped.push(Dropped {
        stage: "gates",
        reason,
    });
}

fn sound(value: f64) -> bool {
    // Written so NaN fails too.
    (0.0..=1.0).contains(&value)
}

/// Whether a finding at this severity and (unrounded) confidence may publish.
fn publishes(
    strictness: u8,
    severity: Severity,
    style: bool,
    confidence: f64,
) -> Result<(), &'static str> {
    if confidence < MEDIUM {
        return Err("low_confidence");
    }
    if severity == Severity::P2 && confidence < HIGH {
        return Err("medium_confidence_p2");
    }
    match (severity, style, strictness) {
        (Severity::P2, _, 3..) => Err("strictness"),
        (Severity::P2, true, 2) if confidence < STYLE_BAR => Err("strictness"),
        _ => Ok(()),
    }
}

/// `verbose` is the lowest strictness among the reviewed files: the most verbose config
/// in the PR sets the PR-wide caps (CONFIGURATION §4.1), whether or not its file has a
/// finding.
pub fn run(judged: Vec<Judged>, policy: &dyn Fn(&RepoPath) -> Policy, verbose: u8) -> Gated {
    let mut out = Gated::default();
    let mut questions: Vec<(Severity, f64, Question)> = Vec::new();
    for j in judged {
        let Judged { mut item, judge } = j;
        let id = item.id.clone();
        let (own, assessed) = (item.candidate.confidence, judge.confidence);
        if !(sound(own) && sound(assessed)) {
            drop(&mut out, "malformed_value");
            continue;
        }
        if judge.verdict == Verdict::Reject {
            drop(&mut out, "judge_rejected");
            continue;
        }
        // Security findings are always `logic` (REVIEW_RULES §3).
        if item.candidate.category == Category::Security {
            item.candidate.comment_type = Kind::Logic;
        }
        let strictness = policy(&item.config_path).strictness;
        // The judge may lower a severity, never raise it; style is P2 only; a rule's
        // severity is a floor, so it can raise (REVIEW_RULES §2, §3, §10).
        let mut severity = item
            .candidate
            .severity
            .max(judge.severity.unwrap_or(item.candidate.severity));
        let style = item.candidate.comment_type == Kind::Style;
        if style {
            severity = Severity::P2;
        }
        if let Some(floor) = item.floor {
            severity = severity.min(floor);
        }
        // The lower assessment stands: the second opinion can only make it less sure.
        let confidence = own.min(assessed);
        if judge.verdict == Verdict::NeedsFollowUp {
            let ask = severity == Severity::P0 || (severity == Severity::P1 && strictness < 3);
            match judge.missing_fact {
                Some(fact) if ask && confidence >= MEDIUM => {
                    let c = &item.candidate;
                    questions.push((
                        severity,
                        confidence,
                        Question {
                            // The finding's id, so two issues on one line ask two questions.
                            id: format!("q_{}", id.strip_prefix("f_").unwrap_or(&id)),
                            path: c.path.clone(),
                            start_line: c.start_line,
                            end_line: c.end_line,
                            side: match c.side {
                                SideOut::Old => Side::Old,
                                SideOut::New => Side::New,
                            },
                            body: fact,
                            why: judge.reason,
                        },
                    ));
                }
                _ => drop(&mut out, "follow_up_unusable"),
            }
            continue;
        }
        match publishes(strictness, severity, style, confidence) {
            Ok(()) => out.findings.push(Final {
                item,
                id,
                severity,
                confidence,
            }),
            Err(reason) => drop(&mut out, reason),
        }
    }
    out.findings.sort_by(|a, b| {
        let at = |f: &Final| {
            (
                f.severity,
                f.item.candidate.path.clone(),
                f.item.candidate.start_line,
                f.id.clone(),
            )
        };
        (a.severity, b.confidence.total_cmp(&a.confidence))
            .cmp(&(b.severity, std::cmp::Ordering::Equal))
            .then_with(|| at(a).cmp(&at(b)))
    });
    // The least severe and confident style findings go first (REVIEW_RULES §5.1).
    let cap = if verbose == 1 { 5 } else { 2 };
    let mut styled = 0;
    let mut kept = Vec::new();
    for f in std::mem::take(&mut out.findings) {
        if f.item.candidate.comment_type == Kind::Style {
            styled += 1;
            if styled > cap {
                drop(&mut out, "style_cap");
                continue;
            }
        }
        kept.push(f);
    }
    out.findings = kept;
    questions.sort_by(|a, b| {
        (a.0, b.1.total_cmp(&a.1))
            .cmp(&(b.0, std::cmp::Ordering::Equal))
            .then_with(|| {
                (&a.2.path, a.2.start_line, &a.2.id).cmp(&(&b.2.path, b.2.start_line, &b.2.id))
            })
    });
    let mut seen = std::collections::BTreeSet::new();
    questions.retain(|q| seen.insert(q.2.id.clone()));
    for (n, (_, _, q)) in questions.into_iter().enumerate() {
        if n < MAX_QUESTIONS {
            out.questions.push(q);
        } else {
            drop(&mut out, "question_cap");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use gitcko_prompts::{Category, CommentType as Kind, Verdict};
    use proptest::prelude::*;

    use super::*;
    use crate::validate::fixtures::item;

    /// One file's policy, so the PR-wide strictness is that file's.
    fn run(judged: Vec<Judged>, policy: &dyn Fn(&RepoPath) -> Policy) -> Gated {
        let verbose = policy(&RepoPath::parse("a.rs").unwrap()).strictness;
        super::run(judged, policy, verbose)
    }

    fn policy(strictness: u8) -> impl Fn(&RepoPath) -> Policy {
        move |_| Policy {
            strictness,
            types: vec![CommentType::Logic, CommentType::Syntax, CommentType::Style],
        }
    }

    fn judged(severity: Severity, kind: Kind, confidence: f64, judge_confidence: f64) -> Judged {
        let mut item = item("a.rs", "A bug", Category::Correctness, (2, 3));
        item.candidate.severity = severity;
        item.candidate.comment_type = kind;
        item.candidate.confidence = confidence;
        Judged {
            item,
            judge: JudgeOutput {
                verdict: Verdict::Accept,
                confidence: judge_confidence,
                severity: None,
                reason: "r".into(),
                missing_fact: None,
            },
        }
    }

    fn passes(strictness: u8, severity: Severity, kind: Kind, confidence: f64) -> bool {
        let got = run(
            vec![judged(severity, kind, confidence, confidence)],
            &policy(strictness),
        );
        !got.findings.is_empty()
    }

    #[test]
    fn the_confidence_bands_use_the_unrounded_value() {
        use Severity::{P1, P2};
        // Medium (0.70 to below 0.85) is for P0 and P1 only.
        assert!(!passes(2, P1, Kind::Logic, 0.699_999_9));
        assert!(passes(2, P1, Kind::Logic, 0.70));
        assert!(passes(2, P1, Kind::Logic, 0.849_999_9));
        assert!(!passes(2, P2, Kind::Logic, 0.849_999_9));
        assert!(passes(2, P2, Kind::Logic, 0.85));
        assert!(!passes(2, P2, Kind::Logic, 0.0));
    }

    #[test]
    fn the_strictness_matrix() {
        use Severity::{P0, P1, P2};
        // (strictness, severity, type, confidence, published)
        let table = [
            (1, P0, Kind::Logic, 0.70, true),
            (2, P1, Kind::Syntax, 0.70, true),
            (3, P1, Kind::Logic, 0.70, true),
            (3, P0, Kind::Syntax, 0.85, true),
            (1, P2, Kind::Logic, 0.85, true),
            (2, P2, Kind::Syntax, 0.85, true),
            (3, P2, Kind::Logic, 0.99, false),
            (3, P2, Kind::Syntax, 0.99, false),
            (1, P2, Kind::Style, 0.85, true),
            (1, P2, Kind::Style, 0.849, false),
            (2, P2, Kind::Style, 0.92, true),
            (2, P2, Kind::Style, 0.919_999, false),
            (3, P2, Kind::Style, 0.99, false),
            // Style is P2 only: a style P1 is read as the P2 it is, then held to P2's bar.
            (2, P1, Kind::Style, 0.90, false),
            (2, P1, Kind::Style, 0.92, true),
            (3, P0, Kind::Style, 0.99, false),
        ];
        for (strictness, severity, kind, confidence, want) in table {
            assert_eq!(
                passes(strictness, severity, kind, confidence),
                want,
                "s{strictness} {severity:?} {kind:?} {confidence}"
            );
        }
    }

    #[test]
    fn the_judge_can_only_lower_what_the_candidate_claimed() {
        let mut lowered = judged(Severity::P0, Kind::Logic, 0.9, 0.9);
        lowered.judge.severity = Some(Severity::P2);
        let got = run(vec![lowered], &policy(2));
        assert_eq!(got.findings[0].severity, Severity::P2);
        let mut raised = judged(Severity::P2, Kind::Logic, 0.9, 0.9);
        raised.judge.severity = Some(Severity::P0);
        let got = run(vec![raised], &policy(2));
        assert_eq!(got.findings[0].severity, Severity::P2);
        // Re-checked after normalising: a medium-confidence P1 lowered to P2 is out.
        let mut medium = judged(Severity::P1, Kind::Logic, 0.75, 0.75);
        medium.judge.severity = Some(Severity::P2);
        let got = run(vec![medium], &policy(2));
        assert!(got.findings.is_empty());
        assert_eq!(got.dropped[0].reason, "medium_confidence_p2");
    }

    #[test]
    fn the_lower_of_the_two_confidences_decides() {
        let got = run(
            vec![judged(Severity::P1, Kind::Logic, 0.95, 0.75)],
            &policy(2),
        );
        assert_eq!(got.findings[0].confidence, 0.75);
        let got = run(
            vec![judged(Severity::P1, Kind::Logic, 0.95, 0.5)],
            &policy(2),
        );
        assert!(got.findings.is_empty());
        assert_eq!(got.dropped[0].reason, "low_confidence");
    }

    #[test]
    fn a_confidence_that_is_not_a_number_is_never_published() {
        for bad in [f64::NAN, f64::INFINITY, -0.1, 1.01] {
            let got = run(
                vec![judged(Severity::P0, Kind::Logic, bad, 0.9)],
                &policy(1),
            );
            assert!(got.findings.is_empty(), "{bad}");
            assert_eq!(got.dropped[0].reason, "malformed_value", "{bad}");
        }
        // Positive control: the same finding with a real confidence goes through.
        assert_eq!(
            run(
                vec![judged(Severity::P0, Kind::Logic, 0.9, 0.9)],
                &policy(1)
            )
            .findings
            .len(),
            1
        );
    }

    #[test]
    fn a_rejection_publishes_nothing_and_says_so() {
        let mut rejected = judged(Severity::P0, Kind::Logic, 0.99, 0.99);
        rejected.judge.verdict = Verdict::Reject;
        let got = run(vec![rejected], &policy(1));
        assert!(got.findings.is_empty() && got.questions.is_empty());
        assert_eq!(got.dropped[0].reason, "judge_rejected");
    }

    #[test]
    fn a_security_finding_is_always_logic() {
        let mut security = judged(Severity::P1, Kind::Style, 0.9, 0.9);
        security.item.candidate.category = Category::Security;
        let got = run(vec![security], &policy(2));
        assert_eq!(got.findings[0].item.candidate.comment_type, Kind::Logic);
    }

    #[test]
    fn style_is_capped_by_strictness_and_the_best_survive() {
        for (strictness, cap) in [(2u8, 2usize), (1, 5)] {
            // One more than the cap, then exactly the cap.
            for count in [cap + 1, cap] {
                let batch: Vec<Judged> = (0..count)
                    .map(|n| {
                        let mut j =
                            judged(Severity::P2, Kind::Style, 0.93 + n as f64 / 1000.0, 1.0);
                        j.item.candidate.title = format!("Style {n}");
                        j.item.code = format!("c{n}");
                        j
                    })
                    .collect();
                let got = run(batch, &policy(strictness));
                assert_eq!(got.findings.len(), cap, "s{strictness} n{count}");
                if count > cap {
                    // The least confident one went; the cap names itself.
                    assert_eq!(got.dropped[0].reason, "style_cap");
                    assert!(
                        got.findings
                            .iter()
                            .all(|f| f.item.candidate.title != "Style 0")
                    );
                }
            }
        }
        // Positive control: the cap is for style; logic bugs aren't counted.
        let many: Vec<Judged> = (0..6)
            .map(|n| {
                let mut j = judged(Severity::P1, Kind::Logic, 0.9, 0.9);
                j.item.code = format!("c{n}");
                j
            })
            .collect();
        assert_eq!(run(many, &policy(2)).findings.len(), 6);
    }

    fn question(severity: Severity, confidence: f64, n: usize) -> Judged {
        let mut j = judged(severity, Kind::Logic, confidence, confidence);
        j.judge.verdict = Verdict::NeedsFollowUp;
        j.judge.missing_fact = Some("Is this endpoint public?".into());
        j.item.code = format!("c{n}");
        // Ids are assigned before the gates, one per issue.
        j.item.id = format!("f_{n:016x}");
        j
    }

    #[test]
    fn a_follow_up_on_a_p0_or_p1_becomes_a_question_and_never_a_finding() {
        let got = run(vec![question(Severity::P1, 0.8, 0)], &policy(2));
        assert!(got.findings.is_empty());
        let [q] = got.questions.as_slice() else {
            panic!("{:?}", got.questions)
        };
        assert!(q.id.starts_with("q_") && q.id.len() == 18, "{}", q.id);
        assert_eq!(q.body, "Is this endpoint public?");
        assert_eq!(q.why, "r");
        assert_eq!((q.start_line, q.end_line), (2, 3));
        // A P2, and a doubt below the confidence bar, are not worth a person's time.
        for j in [
            question(Severity::P2, 0.95, 0),
            question(Severity::P1, 0.69, 0),
        ] {
            let got = run(vec![j], &policy(2));
            assert!(got.questions.is_empty());
            assert_eq!(got.dropped[0].reason, "follow_up_unusable");
        }
    }

    #[test]
    fn strictness_three_asks_only_about_p0() {
        assert!(
            run(vec![question(Severity::P1, 0.9, 0)], &policy(3))
                .questions
                .is_empty()
        );
        assert_eq!(
            run(vec![question(Severity::P0, 0.9, 0)], &policy(3))
                .questions
                .len(),
            1
        );
    }

    #[test]
    fn at_most_two_questions_and_the_most_severe_win() {
        let three = vec![
            question(Severity::P1, 0.9, 0),
            question(Severity::P0, 0.75, 1),
            question(Severity::P1, 0.8, 2),
        ];
        let got = run(three.clone(), &policy(2));
        assert_eq!(got.questions.len(), 2);
        assert_eq!(
            got.dropped
                .iter()
                .filter(|d| d.reason == "question_cap")
                .count(),
            1
        );
        // The dropped one is the least severe and least confident of the three.
        let kept: Vec<&str> = got.questions.iter().map(|q| q.id.as_str()).collect();
        let want = run(vec![three[1].clone(), three[0].clone()], &policy(2));
        assert_eq!(
            kept,
            want.questions
                .iter()
                .map(|q| q.id.as_str())
                .collect::<Vec<_>>()
        );
        // Exactly the cap drops nothing.
        let two = run(three[..2].to_vec(), &policy(2));
        assert_eq!(two.questions.len(), 2);
        assert!(two.dropped.is_empty());
    }

    #[test]
    fn findings_come_most_severe_then_most_confident_first_with_distinct_ids() {
        let named = |severity, confidence, title: &str| {
            let mut j = judged(severity, Kind::Logic, confidence, confidence);
            j.item.candidate.title = title.into();
            j.item.id = format!("f_{title}");
            j
        };
        let a = named(Severity::P1, 0.9, "a");
        let b = named(Severity::P0, 0.75, "b");
        let c = named(Severity::P1, 0.95, "c");
        let got = run(vec![a, b, c], &policy(2));
        let order: Vec<&str> = got
            .findings
            .iter()
            .map(|f| f.item.candidate.title.as_str())
            .collect();
        assert_eq!(order, ["b", "c", "a"]);
        // Each finding keeps the id it was given.
        let mut ids: Vec<&str> = got.findings.iter().map(|f| f.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3);
    }

    proptest! {
        #[test]
        fn the_outcome_does_not_depend_on_the_order_of_the_judged(
            specs in proptest::collection::vec((0u8..3, 0u8..3, 60u32..100, 0u8..3), 0..9),
            rotate in 0usize..9,
            strictness in 1u8..4,
        ) {
            let batch: Vec<Judged> = specs
                .iter()
                .enumerate()
                .map(|(n, (sev, kind, conf, verdict))| {
                    let severity = [Severity::P0, Severity::P1, Severity::P2][*sev as usize];
                    let kind = [Kind::Logic, Kind::Syntax, Kind::Style][*kind as usize];
                    let mut j = judged(severity, kind, f64::from(*conf) / 100.0, 0.99);
                    j.item.candidate.title = format!("t{}", n % 4);
                    j.item.code = format!("c{}", n % 3);
                    j.item.id = format!("f_{n:016x}");
                    j.judge.verdict = [Verdict::Accept, Verdict::Reject, Verdict::NeedsFollowUp][*verdict as usize];
                    j.judge.missing_fact = Some("fact".into());
                    j
                })
                .collect();
            let mut other = batch.clone();
            if !other.is_empty() {
                let by = rotate % other.len();
                other.rotate_left(by);
                other.reverse();
            }
            let show = |g: Gated| format!("{:?}|{:?}|{:?}", g.findings, g.questions, g.dropped);
            let mut a = run(batch, &policy(strictness));
            let mut b = run(other, &policy(strictness));
            // What was dropped is a set; its order is the order they arrived in.
            a.dropped.sort_by_key(|d| (d.stage, d.reason));
            b.dropped.sort_by_key(|d| (d.stage, d.reason));
            prop_assert_eq!(show(a), show(b));
        }
    }
}
