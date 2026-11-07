// SPDX-License-Identifier: AGPL-3.0-or-later

//! Cross-run lifecycle: finding ids map to new / still_present / resolved, in any order.

use gitcko_protocol::review::{Finding, Lifecycle, Review};
use gitcko_storage::reconcile;
use proptest::prelude::*;

const SAMPLE: &str = include_str!("../../gitcko-protocol/tests/data/review-full.json");

fn finding(id: &str) -> Finding {
    let review: Review = serde_json::from_str(SAMPLE).unwrap();
    let mut f = review.findings.into_iter().next().unwrap();
    f.id = id.to_string();
    f.lifecycle = Lifecycle::New;
    f
}

#[test]
fn previous_ids_still_in_current_are_still_present_and_new_ids_are_new() {
    let previous = vec![finding("f_aaaaaaaaaaaaaaaa"), finding("f_bbbbbbbbbbbbbbbb")];
    let mut current = vec![finding("f_bbbbbbbbbbbbbbbb"), finding("f_cccccccccccccccc")];
    let resolved = reconcile(&previous, &mut current);
    assert_eq!(resolved, ["f_aaaaaaaaaaaaaaaa"]);
    assert_eq!(current[0].lifecycle, Lifecycle::StillPresent);
    assert_eq!(current[1].lifecycle, Lifecycle::New);
}

#[test]
fn reconcile_does_not_emit_possibly_fixed() {
    let previous = vec![finding("f_aaaaaaaaaaaaaaaa")];
    let mut current = vec![];
    let resolved = reconcile(&previous, &mut current);
    assert_eq!(resolved, ["f_aaaaaaaaaaaaaaaa"]);
    assert!(current.is_empty());
}

proptest! {
    #[test]
    fn reconcile_matches_set_difference_in_any_order(
        prev in prop::collection::vec(finding_id(), 0..8),
        curr in prop::collection::vec(finding_id(), 0..8),
    ) {
        let previous: Vec<_> = prev.iter().map(|id| finding(id)).collect();
        let mut current: Vec<_> = curr.iter().map(|id| finding(id)).collect();
        let resolved = reconcile(&previous, &mut current);
        let prev_set: std::collections::BTreeSet<_> = prev.iter().cloned().collect();
        let curr_set: std::collections::BTreeSet<_> = curr.iter().cloned().collect();
        let expect_resolved: Vec<_> = prev_set.difference(&curr_set).cloned().collect();
        let mut got = resolved;
        got.sort();
        let mut expect = expect_resolved;
        expect.sort();
        prop_assert_eq!(got, expect);
        for f in &current {
            if prev_set.contains(&f.id) {
                prop_assert_eq!(f.lifecycle, Lifecycle::StillPresent);
            } else {
                prop_assert_eq!(f.lifecycle, Lifecycle::New);
            }
            prop_assert_ne!(f.lifecycle, Lifecycle::PossiblyFixed);
            prop_assert_ne!(f.lifecycle, Lifecycle::Resolved);
        }
    }
}

fn finding_id() -> impl Strategy<Value = String> {
    "[0-9a-f]{16}".prop_map(|hex| format!("f_{hex}"))
}
