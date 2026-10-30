// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the engine draws on besides the model (REVIEW_ENGINE §7, §21.1), each with a
//! typed status, so "no signals" and "the signals failed" stay different answers.

use std::collections::BTreeSet;

use gitcko_diff::LineKind;

use crate::snapshot::Snapshot;
use crate::symbols::ChangedSymbol;

/// How a provider's answer came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStatus {
    Ok,
    Disabled,
    Unavailable,
    Error,
}

/// A deterministic reason to look harder at something. `text` holds repository names, so
/// it only ever reaches a model fenced.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Signal {
    pub path: String,
    pub text: String,
    /// Worth a hypothesis of its own: a public item changed and other code calls it.
    pub seeds_hypothesis: bool,
    pub high_risk: bool,
}

/// Repository rules and facts the prompts carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Knowledge {
    pub rules: Vec<String>,
}

pub trait KnowledgeProvider {
    fn knowledge(&self) -> (ProviderStatus, Knowledge);
}

pub trait RiskSignalProvider {
    fn signals(&self) -> (ProviderStatus, Vec<Signal>);
}

/// Past human verdicts on findings.
pub trait FeedbackProvider {
    /// Fingerprints of findings a person has dismissed.
    fn dismissed(&self) -> (ProviderStatus, Vec<String>);
}

/// Feedback doesn't exist yet, and says so instead of answering "none".
pub struct NoFeedback;

impl FeedbackProvider for NoFeedback {
    fn dismissed(&self) -> (ProviderStatus, Vec<String>) {
        (ProviderStatus::Disabled, Vec::new())
    }
}

/// `.gitcko/` rules from the base commit.
pub struct RepoKnowledge<'a> {
    pub snapshot: &'a Snapshot<'a>,
}

impl KnowledgeProvider for RepoKnowledge<'_> {
    fn knowledge(&self) -> (ProviderStatus, Knowledge) {
        let rules = self
            .snapshot
            .config
            .rules_md
            .iter()
            .map(|(text, _)| text.clone());
        let mut scoped = BTreeSet::new();
        for path in self.snapshot.investigated() {
            for rule in self.snapshot.config.rules_for(path) {
                if rule.enabled != Some(false) {
                    scoped.insert(rule.rule);
                }
            }
        }
        (
            ProviderStatus::Ok,
            Knowledge {
                rules: rules.chain(scoped).collect(),
            },
        )
    }
}

/// Words in added lines worth a closer look, by what they tend to mean.
const RISKY_WORDS: [&str; 9] = [
    "unsafe", "lock(", "password", "token", "secret", "auth", "crypto", "sql", "select ",
];

/// Signals from the graph and the diff.
pub struct GraphSignals<'a> {
    pub snapshot: &'a Snapshot<'a>,
    pub changed: &'a [ChangedSymbol],
}

impl RiskSignalProvider for GraphSignals<'_> {
    fn signals(&self) -> (ProviderStatus, Vec<Signal>) {
        let mut out = BTreeSet::new();
        for symbol in self.changed {
            if symbol.public && symbol.callers > 0 {
                out.insert(Signal {
                    path: symbol.path.clone(),
                    text: format!(
                        "{}: public {} changed; {} caller(s) elsewhere",
                        symbol.path, symbol.qualified_name, symbol.callers
                    ),
                    seeds_hypothesis: true,
                    high_risk: false,
                });
            }
        }
        let allowed = self.snapshot.investigated();
        for file in &self.snapshot.change.files {
            let Some(path) = file.new_path.as_ref().filter(|p| allowed.contains(p)) else {
                continue;
            };
            let added = file
                .hunks
                .iter()
                .flat_map(|h| &h.lines)
                .filter(|l| l.kind == LineKind::Added);
            for line in added {
                let lower = line.text.to_lowercase();
                if let Some(word) = RISKY_WORDS.iter().find(|w| lower.contains(**w)) {
                    out.insert(Signal {
                        path: path.as_str().to_string(),
                        text: format!("{}: added line mentions `{}`", path.as_str(), word.trim()),
                        seeds_hypothesis: false,
                        high_risk: true,
                    });
                }
            }
        }
        (ProviderStatus::Ok, out.into_iter().collect())
    }
}

/// The signals, or none when the provider failed: planning goes on without them.
pub fn signals_or_empty(provider: &dyn RiskSignalProvider) -> Vec<Signal> {
    match provider.signals() {
        (ProviderStatus::Ok, signals) => signals,
        _ => Vec::new(),
    }
}
