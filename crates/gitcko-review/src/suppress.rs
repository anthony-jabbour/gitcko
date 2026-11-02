// SPDX-License-Identifier: AGPL-3.0-or-later

//! Suppression and rules (REVIEW_RULES §5–6, §10). Rules are read from the base commit,
//! so a change can't loosen its own review. What the never-suppressed list names can't be
//! removed here by a file type, a generated marker, a rule or past feedback.

use std::collections::BTreeSet;

use gitcko_domain::{RepoPath, Side};
use gitcko_prompts::{Category, Severity as Model};
use gitcko_protocol::review::Severity;

use crate::gates::policy;
use crate::providers::{FeedbackProvider, ProviderStatus};
use crate::snapshot::Snapshot;
use crate::validate::{Dropped, Item, Reader};

/// REVIEW_RULES §6, as far as the baseline categories reach: P0, security, data loss,
/// and leaks. Null dereference, infinite loops and missing input validation have no
/// category of their own to name them by, so they are suppressible like anything else.
/// A rule's P0 floor counts: the finding publishes at P0 whatever was proposed.
pub fn never_suppressed(item: &Item) -> bool {
    item.candidate.severity == Model::P0
        || item.floor == Some(Model::P0)
        || matches!(
            item.candidate.category,
            Category::Security | Category::DataIntegrity | Category::ResourceManagement
        )
}

/// Files that say a person doesn't maintain them.
const GENERATED: [&str; 2] = ["@generated", "DO NOT EDIT"];
const MARKER_LINES: usize = 20;

fn model_severity(severity: Severity) -> Model {
    match severity {
        Severity::P0 => Model::P0,
        Severity::P1 => Model::P1,
        Severity::P2 => Model::P2,
    }
}

/// Whether the comment types in force for the item's file include its own.
pub fn type_allowed(snapshot: &Snapshot<'_>, item: &Item) -> bool {
    let kind = match item.candidate.comment_type {
        gitcko_prompts::CommentType::Logic => gitcko_protocol::review::CommentType::Logic,
        gitcko_prompts::CommentType::Syntax => gitcko_protocol::review::CommentType::Syntax,
        gitcko_prompts::CommentType::Style => gitcko_protocol::review::CommentType::Style,
    };
    policy(snapshot, &item.config_path).types.contains(&kind)
}

pub fn run(
    items: Vec<Item>,
    snapshot: &Snapshot<'_>,
    feedback: &dyn FeedbackProvider,
) -> (Vec<Item>, Vec<Dropped>) {
    let mut reader = Reader::new(snapshot);
    // A source that isn't up makes no claim, so it silences nothing.
    let dismissed: BTreeSet<String> = match feedback.dismissed() {
        (ProviderStatus::Ok, ids) => ids.into_iter().collect(),
        _ => BTreeSet::new(),
    };
    let (mut kept, mut dropped) = (Vec::new(), Vec::new());
    for mut item in items {
        // A rule's severity floor decides whether the finding is protected, so the rule is
        // resolved before anything may suppress it.
        if item.candidate.category == Category::RepositoryRule && !link_rule(&mut item, snapshot) {
            dropped.push(Dropped {
                stage: "suppress",
                reason: "no_rule_in_force",
            });
            continue;
        }
        let protected = never_suppressed(&item);
        let side = match item.candidate.side {
            gitcko_prompts::SideOut::Old => Side::Old,
            gitcko_prompts::SideOut::New => Side::New,
        };
        let generated = reader
            .lines(side, &item.candidate.path)
            .is_some_and(|lines| {
                lines
                    .iter()
                    .take(MARKER_LINES)
                    .any(|l| GENERATED.iter().any(|m| l.contains(m)))
            });
        let verdict = if generated && !protected {
            Err("generated_file")
        } else if !protected && !type_allowed(snapshot, &item) {
            Err("comment_type_disabled")
        } else if !protected && dismissed.contains(&item.id) {
            Err("dismissed")
        } else {
            Ok(())
        };
        match verdict {
            Ok(()) => kept.push(item),
            Err(reason) => dropped.push(Dropped {
                stage: "suppress",
                reason,
            }),
        }
    }
    (kept, dropped)
}

/// Ties a rule finding to the rule it enforces. A candidate doesn't name its rule, so the
/// engine only claims one when one is possible: a lone `[[rules]]` entry, with its id and
/// severity floor. Beside other rules or a `rules.md` the finding stands unlinked, since a
/// wrong link would misstate the finding and raise its severity. False when no rule at all
/// is in force.
fn link_rule(item: &mut Item, snapshot: &Snapshot<'_>) -> bool {
    let Some(config) = snapshot.configs.get(&item.config_path) else {
        return false;
    };
    let Ok(path) = RepoPath::parse(&item.candidate.path) else {
        return false;
    };
    let in_force: Vec<_> = config
        .rules_for(&path)
        .into_iter()
        .filter(|r| r.enabled != Some(false))
        .collect();
    // The judge reads what the finding answers to, not the model's account of it.
    item.rules = config
        .rules_md
        .iter()
        .map(|(text, _)| text.clone())
        .chain(in_force.iter().map(|r| r.rule.clone()))
        .collect();
    let mut rules = in_force.into_iter();
    match (rules.next(), rules.next()) {
        (Some(rule), None) if config.rules_md.is_empty() => {
            item.rule = rule.id;
            item.floor = rule.severity.map(model_severity);
            true
        }
        (None, _) => !config.rules_md.is_empty(),
        _ => true,
    }
}
