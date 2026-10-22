// SPDX-License-Identifier: AGPL-3.0-or-later

//! CONFIGURATION §4.1: when a PR touches files under different configs, PR-wide values
//! combine the per-file configs.

use gitcko_config::combine_for_pr;
use gitcko_protocol::config::{AutoApprove, AutoApproveFilters, AutoReviewTrigger, RepoConfig};
use gitcko_protocol::review::{CommentType, RiskLevel, Severity, Tier};

fn config(toml: &str) -> RepoConfig {
    toml::from_str(toml).unwrap()
}

#[test]
fn most_verbose_strictness_highest_tier_union_of_types() {
    let a = config(
        "strictness = 3\ntier = \"base\"\ncomment_types = [\"logic\"]\nauto_review = [\"open\"]",
    );
    let b = config(
        "strictness = 1\ntier = \"plus\"\ncomment_types = [\"style\"]\nauto_review = [\"push\"]",
    );
    let pr = combine_for_pr(&[a, b]);
    assert_eq!(pr.strictness, Some(1));
    assert_eq!(pr.tier, Some(Tier::Plus));
    assert_eq!(
        pr.comment_types,
        Some(vec![CommentType::Logic, CommentType::Style])
    );
    assert_eq!(
        pr.auto_review,
        Some(vec![AutoReviewTrigger::Open, AutoReviewTrigger::Push])
    );
}

#[test]
fn auto_tier_counts_only_when_no_config_names_one() {
    let auto = config("tier = \"auto\"");
    let named = config("tier = \"base\"");
    assert_eq!(
        combine_for_pr(&[auto.clone(), named]).tier,
        Some(Tier::Base)
    );
    assert_eq!(
        combine_for_pr(&[auto, RepoConfig::default()]).tier,
        Some(Tier::Auto)
    );
}

#[test]
fn booleans_or_but_skip_needs_every_config() {
    let a = config("check_run = false\nreview_drafts = true\nskip_auto_review = true");
    let b = config("check_run = true\nskip_auto_review = false");
    let pr = combine_for_pr(&[a.clone(), b]);
    assert_eq!(pr.check_run, Some(true));
    assert_eq!(pr.review_drafts, Some(true));
    assert_eq!(pr.skip_auto_review, Some(false));
    assert_eq!(combine_for_pr(&[a.clone(), a]).skip_auto_review, Some(true));
}

#[test]
fn fail_on_takes_the_most_severe_threshold() {
    let pr = combine_for_pr(&[
        config("fail_on = \"P2\""),
        config("fail_on = \"P0\""),
        RepoConfig::default(),
    ]);
    assert_eq!(pr.fail_on, Some(Severity::P0));
}

#[test]
fn auto_approve_is_strictest_wins() {
    let a = config(
        "[auto_approve]\nenabled = true\nrisk_ceiling = \"medium\"\n[auto_approve.filters]\nexclude_paths = [\"a/**\"]\ninclude_paths = [\"src/**\", \"lib/**\"]",
    );
    let b = config(
        "[auto_approve]\nenabled = true\nrisk_ceiling = \"low\"\n[auto_approve.filters]\nexclude_paths = [\"b/**\"]\ninclude_paths = [\"src/**\"]",
    );
    let pr = combine_for_pr(&[a.clone(), b]);
    assert_eq!(
        pr.auto_approve,
        Some(AutoApprove {
            enabled: Some(true),
            risk_ceiling: Some(RiskLevel::Low),
            filters: Some(AutoApproveFilters {
                exclude_paths: Some(vec!["a/**".into(), "b/**".into()]),
                include_paths: Some(vec!["src/**".into()]),
                exclude_authors: None,
            }),
        })
    );
    // Enabled only if every touched config enables it.
    let off = config("[auto_approve]\nenabled = false");
    assert_eq!(
        combine_for_pr(&[a, off]).auto_approve.unwrap().enabled,
        Some(false)
    );
}

#[test]
fn summary_section_shows_if_any_config_enables_it() {
    let a = config("[summary.diagram]\nincluded = false");
    let b = config("[summary.diagram]\nincluded = true");
    let pr = combine_for_pr(&[a, b]);
    assert_eq!(pr.summary.unwrap().diagram.unwrap().included, Some(true));
}
