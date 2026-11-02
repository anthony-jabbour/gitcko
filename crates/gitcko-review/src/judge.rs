// SPDX-License-Identifier: AGPL-3.0-or-later

//! Semantic adjudication (REVIEW_ENGINE §14). P4 has no deterministic causal validator,
//! so every candidate that passed validation, dedup and suppression goes to the `judge`
//! role: a separate call and context, on another model family when the router can.
//! Nothing is published on its own say-so, and a candidate the judge never answered for
//! stays unpublished with the run incomplete: no fail-open.

use gitcko_model::CallError;
use gitcko_prompts::{JudgeOutput, judge};
use gitcko_protocol::config::ModelRole;
use gitcko_tools::Ledger;

use crate::complete::{Completeness, reason};
use crate::gates::Judged;
use crate::investigate::{Asked, Calls, refuse};
use crate::validate::{Dropped, Item};

fn unjudged() -> Dropped {
    Dropped {
        stage: "adjudicate",
        reason: "unjudged",
    }
}

/// The candidate as the judge reads it. Repository text and the model's own: it is fenced
/// by the prompt.
fn render(item: &Item) -> String {
    let c = &item.candidate;
    let mut out = format!(
        "Title: {}\nCategory: {:?}\nProposed severity: {:?}\nLocation: {} lines {}-{} ({:?} side)\nTrigger: {}\nCause: {}\nImpact: {}",
        c.title,
        c.category,
        c.severity,
        c.path,
        c.start_line,
        c.end_line,
        c.side,
        c.trigger,
        c.cause,
        c.impact
    );
    if let Some(link) = &item.outside {
        out.push_str(&format!(
            "\nOutside the diff; traces to {} line {}",
            link.linked_path, link.linked_line
        ));
    }
    for place in &item.also {
        out.push_str(&format!(
            "\nAlso reported at {} lines {}-{}",
            place.path, place.start_line, place.end_line
        ));
    }
    if !item.rules.is_empty() {
        out.push_str("\nRepository rules in force here, from the base commit:");
        for rule in &item.rules {
            out.push_str(&format!("\n- {rule}"));
        }
    }
    for title in &item.overlaps {
        out.push_str(&format!(
            "\nOverlaps another candidate, which may or may not be the same issue: {title}"
        ));
    }
    out
}

/// The cited ranges, for and against, as `(id, text)`.
fn cited(item: &Item, evidence: &Ledger) -> Vec<(String, String)> {
    let c = &item.candidate;
    let mut ids: Vec<&String> = c.evidence.iter().chain(&c.counterevidence).collect();
    ids.dedup();
    ids.into_iter()
        .filter_map(|id| evidence.get(id))
        .map(|e| {
            let side = format!("{:?}", e.range.side());
            let against = c.counterevidence.contains(&e.id);
            (
                e.id.clone(),
                format!(
                    "{} lines {}-{} ({side} side){}\n{}",
                    e.path.as_str(),
                    e.range.start(),
                    e.range.end(),
                    if against { ", argues against" } else { "" },
                    e.text
                ),
            )
        })
        .collect()
}

/// Asks the judge about each item in order. After a call that fails, the rest are not
/// asked: the router already retried and fell back, and the budget they'd spend is the
/// run's.
pub async fn run(
    calls: &mut Calls<'_, '_>,
    items: Vec<Item>,
    evidence: &Ledger,
    ledger: &mut Completeness,
) -> (Vec<Judged>, Vec<Dropped>) {
    let (mut judged, mut dropped) = (Vec::new(), Vec::new());
    let mut queue = items.into_iter();
    while let Some(item) = queue.next() {
        let request = judge(&render(&item), &cited(&item, evidence));
        let answer = calls.ask::<JudgeOutput>("adjudicate", request).await;
        // An answer that arrives after a cancel isn't used (REVIEW_ENGINE §18).
        let answer = match answer {
            Asked::Parsed(_) | Asked::Invalid if calls.canceled() => {
                Asked::Failed(CallError::Canceled)
            }
            other => other,
        };
        match answer {
            Asked::Parsed(judge) => judged.push(Judged { item, judge }),
            Asked::Invalid => {
                ledger.add(reason::INVALID_MODEL_OUTPUT);
                dropped.push(Dropped {
                    stage: "adjudicate",
                    reason: "judge_invalid",
                });
            }
            failed => {
                match failed {
                    Asked::Failed(error) => ledger.call_failed(ModelRole::Judge, &error, false),
                    _ => refuse(ledger),
                }
                dropped.push(unjudged());
                dropped.extend(queue.by_ref().map(|_| unjudged()));
            }
        }
    }
    (judged, dropped)
}

pub fn all_unjudged(items: &[Item]) -> Vec<Dropped> {
    items.iter().map(|_| unjudged()).collect()
}
