// SPDX-License-Identifier: AGPL-3.0-or-later

//! PR-wide values when a PR touches files under different configs (CONFIGURATION §4.1,
//! the same rules as Greptile's `.greptile/` cascade). Each file is still reviewed with
//! its own config; these are the values that apply to the PR as a whole.

use gitcko_protocol::config::{
    AutoApprove, AutoApproveFilters, RepoConfig, Summary, SummarySection,
};
use gitcko_protocol::review::Tier;

/// Combines the resolved configs of every file the PR touches. Only the PR-wide keys
/// are set in the result.
pub fn combine_for_pr(configs: &[RepoConfig]) -> RepoConfig {
    let all = || configs.iter();
    RepoConfig {
        // Most verbose wins (1 = most verbose).
        strictness: all().filter_map(|c| c.strictness).min(),
        tier: tier(configs),
        comment_types: union(all().map(|c| &c.comment_types)),
        auto_review: union(all().map(|c| &c.auto_review)),
        review_drafts: any(all().map(|c| c.review_drafts)),
        check_run: any(all().map(|c| c.check_run)),
        summary_only: any(all().map(|c| c.summary_only)),
        fix_links: any(all().map(|c| c.fix_links)),
        hide_footer: any(all().map(|c| c.hide_footer)),
        update_description: any(all().map(|c| c.update_description)),
        // Skipped only if every touched config says so.
        skip_auto_review: every(all().map(|c| c.skip_auto_review)),
        // The most severe threshold (P0 < P1 < P2).
        fail_on: all().filter_map(|c| c.fail_on).min(),
        auto_approve: auto_approve(configs),
        summary: summary(configs),
        ..RepoConfig::default()
    }
}

/// The highest named tier; `auto` only when no config names one.
fn tier(configs: &[RepoConfig]) -> Option<Tier> {
    let named = configs
        .iter()
        .filter_map(|c| c.tier)
        .filter(|t| *t != Tier::Auto)
        .max_by_key(|t| match t {
            Tier::Base => 0,
            Tier::Plus => 1,
            _ => 2,
        });
    named.or_else(|| {
        configs
            .iter()
            .any(|c| c.tier == Some(Tier::Auto))
            .then_some(Tier::Auto)
    })
}

fn union<'a, T: Clone + PartialEq + 'a>(
    lists: impl Iterator<Item = &'a Option<Vec<T>>>,
) -> Option<Vec<T>> {
    let mut out: Option<Vec<T>> = None;
    for list in lists.flatten() {
        let merged = out.get_or_insert_with(Vec::new);
        for item in list {
            if !merged.contains(item) {
                merged.push(item.clone());
            }
        }
    }
    out
}

fn intersect<'a, T: Clone + PartialEq + 'a>(
    lists: impl Iterator<Item = &'a Option<Vec<T>>>,
) -> Option<Vec<T>> {
    let mut out: Option<Vec<T>> = None;
    for list in lists.flatten() {
        out = Some(match out {
            None => list.clone(),
            Some(kept) => kept
                .into_iter()
                .filter(|item| list.contains(item))
                .collect(),
        });
    }
    out
}

fn any(values: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    values.flatten().reduce(|a, b| a || b)
}

fn every(values: impl Iterator<Item = Option<bool>>) -> Option<bool> {
    values.flatten().reduce(|a, b| a && b)
}

/// Strictest wins: enabled only if enabled everywhere, excludes union, includes
/// intersect, the lowest risk ceiling.
fn auto_approve(configs: &[RepoConfig]) -> Option<AutoApprove> {
    let all: Vec<&AutoApprove> = configs
        .iter()
        .filter_map(|c| c.auto_approve.as_ref())
        .collect();
    if all.is_empty() {
        return None;
    }
    let filters: Vec<&AutoApproveFilters> = all.iter().filter_map(|a| a.filters.as_ref()).collect();
    Some(AutoApprove {
        enabled: every(all.iter().map(|a| a.enabled)),
        risk_ceiling: all.iter().filter_map(|a| a.risk_ceiling).min(),
        filters: (!filters.is_empty()).then(|| AutoApproveFilters {
            exclude_paths: union(filters.iter().map(|f| &f.exclude_paths)),
            include_paths: intersect(filters.iter().map(|f| &f.include_paths)),
            exclude_authors: union(filters.iter().map(|f| &f.exclude_authors)),
        }),
    })
}

/// A section is shown if any config enables it; its other booleans OR too.
fn summary(configs: &[RepoConfig]) -> Option<Summary> {
    let all: Vec<&Summary> = configs.iter().filter_map(|c| c.summary.as_ref()).collect();
    if all.is_empty() {
        return None;
    }
    let section = |pick: fn(&Summary) -> &Option<SummarySection>| {
        let sections: Vec<&SummarySection> = all.iter().filter_map(|s| pick(s).as_ref()).collect();
        (!sections.is_empty()).then(|| SummarySection {
            included: any(sections.iter().map(|s| s.included)),
            collapsible: any(sections.iter().map(|s| s.collapsible)),
            default_open: any(sections.iter().map(|s| s.default_open)),
        })
    };
    Some(Summary {
        overview: section(|s| &s.overview),
        files_table: section(|s| &s.files_table),
        score: section(|s| &s.score),
        diagram: section(|s| &s.diagram),
        security: section(|s| &s.security),
    })
}
