// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which model family wrote a PR, so review can use another one (model inversion,
//! MODEL_ROUTING §9). The signals are spoofable, and that is fine: they only choose
//! among bindings the user already allowed.

use gitcko_protocol::config::ModelFamily;
use serde::Serialize;

/// What the PR says about its author: commit messages (for trailers), branch and title.
#[derive(Debug, Clone, Default)]
pub struct AuthorSignals {
    pub commit_messages: Vec<String>,
    pub branch: String,
    pub title: String,
}

/// The agent → family table. Part of the routing policy, so it can change without a
/// code change; `Default` is the built-in one. Agents whose model the user picks
/// (Cursor, Copilot, Devin) are absent, so they map to unknown.
#[derive(Debug, Clone)]
pub struct AgentFamilies {
    /// Email domain of a `Co-authored-by` trailer → family.
    pub trailer_domains: Vec<(String, ModelFamily)>,
    /// Branch prefix (`claude/…`) or title tag (`[claude]`) → family.
    pub agent_names: Vec<(String, ModelFamily)>,
}

impl Default for AgentFamilies {
    fn default() -> AgentFamilies {
        AgentFamilies {
            trailer_domains: vec![
                ("anthropic.com".into(), ModelFamily::Anthropic),
                ("openai.com".into(), ModelFamily::Openai),
            ],
            agent_names: vec![
                ("claude".into(), ModelFamily::Anthropic),
                ("codex".into(), ModelFamily::Openai),
            ],
        }
    }
}

/// Which signal decided the family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Trailer,
    BranchPrefix,
    TitlePrefix,
}

/// The inferred author family, or unknown (mixed, conflicting or no signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorFamily {
    Known { family: ModelFamily, signal: Signal },
    Unknown,
}

/// What inversion decided for a call, recorded per run (MODEL_ROUTING §9 item 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InversionDecision {
    /// A binding from a family other than the author's was chosen.
    Applied,
    /// Every review binding is one family, so there's nothing to invert to.
    SingleFamily,
    /// No reliable signal; the default route was used.
    AuthorUnknown,
    /// The author's family was known but no other family's binding was eligible.
    NoOtherFamilyAvailable,
    /// Turned off in `[models.inversion]`.
    Disabled,
}

/// Signals in order: trailers, then branch prefix, then title prefix. Trailers decide
/// when more than half the commits carry one family's; trailers without such a majority
/// make the author unknown rather than falling through to weaker signals.
pub fn author_family(signals: &AuthorSignals, table: &AgentFamilies) -> AuthorFamily {
    // Commits per family, counting only commits whose trailers name exactly one.
    let mut trailer_commits: Vec<(ModelFamily, usize)> = Vec::new();
    let mut any_trailer = false;
    for message in &signals.commit_messages {
        let mut families: Vec<ModelFamily> = Vec::new();
        for family in co_author_domains(message).filter_map(|d| family_of_domain(d, table)) {
            if !families.contains(&family) {
                families.push(family);
            }
        }
        any_trailer |= !families.is_empty();
        if let [family] = families[..] {
            match trailer_commits.iter_mut().find(|(f, _)| *f == family) {
                Some((_, n)) => *n += 1,
                None => trailer_commits.push((family, 1)),
            }
        }
    }
    let total = signals.commit_messages.len();
    if let Some((family, _)) = trailer_commits.iter().find(|(_, n)| n * 2 > total) {
        return AuthorFamily::Known {
            family: *family,
            signal: Signal::Trailer,
        };
    }
    if any_trailer {
        return AuthorFamily::Unknown;
    }
    let branch_agent = signals.branch.split_once('/').map(|(agent, _)| agent);
    let title_agent = signals
        .title
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
        .map(|(agent, _)| agent);
    for (agent, signal) in [
        (branch_agent, Signal::BranchPrefix),
        (title_agent, Signal::TitlePrefix),
    ] {
        if let Some(family) = agent.and_then(|a| family_of_agent(a, table)) {
            return AuthorFamily::Known { family, signal };
        }
    }
    AuthorFamily::Unknown
}

/// Email domains of `Co-authored-by:` lines in the message's last paragraph, where
/// git reads trailers; a trailer quoted in the body doesn't count. The key is matched
/// case-insensitively, as git does.
fn co_author_domains(message: &str) -> impl Iterator<Item = &str> {
    // `lines` also strips `\r`, and a blank line may hold spaces.
    let lines: Vec<&str> = message.lines().collect();
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1);
    let start = lines[..end]
        .iter()
        .rposition(|l| l.trim().is_empty())
        .map_or(0, |i| i + 1);
    lines.into_iter().take(end).skip(start).filter_map(|line| {
        let (key, value) = line.split_once(':')?;
        if !key.trim().eq_ignore_ascii_case("co-authored-by") {
            return None;
        }
        let email = value.rsplit_once('<')?.1.split_once('>')?.0;
        Some(email.rsplit_once('@')?.1)
    })
}

/// The domain itself or a subdomain of it (`mail.anthropic.com`), never a name that
/// merely starts with it (`anthropic.com.evil.test`).
fn family_of_domain(domain: &str, table: &AgentFamilies) -> Option<ModelFamily> {
    let domain = domain.to_ascii_lowercase();
    table.trailer_domains.iter().find_map(|(known, family)| {
        let sub = domain
            .strip_suffix(known.as_str())
            .is_some_and(|rest| rest.ends_with('.'));
        (domain == *known || sub).then_some(*family)
    })
}

fn family_of_agent(agent: &str, table: &AgentFamilies) -> Option<ModelFamily> {
    table
        .agent_names
        .iter()
        .find_map(|(name, family)| agent.eq_ignore_ascii_case(name).then_some(*family))
}
