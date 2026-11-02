// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deduplication (REVIEW_ENGINE §13). Grouping is a search heuristic, not proof: only
//! what is certainly one root cause is merged; lines that merely overlap are told to the
//! judge and stay separate.

use std::cmp::Reverse;

use gitcko_prompts::Severity;
use gitcko_protocol::review::{Evidence, EvidenceKind};

use crate::identity::title_signature;
use crate::validate::Item;

/// A total order over everything a candidate says, so the pass below sees the same
/// sequence however the candidates arrived.
fn key(item: &Item) -> String {
    let c = &item.candidate;
    format!(
        "{}\0{:?}\0{:010}\0{:010}\0{:?}\0{}\0{:?}\0{}\0{}\0{}\0{:?}",
        c.path,
        c.side,
        c.start_line,
        c.end_line,
        c.category,
        c.title,
        c.severity,
        c.confidence.to_bits(),
        c.cause,
        c.trigger,
        c.evidence
    )
}

/// The report to keep as the finding: the most severe, then the most confident.
fn rank(item: &Item) -> (Severity, Reverse<u64>, String) {
    // Confidences are in [0, 1], where the bit pattern orders like the number.
    (
        item.candidate.severity,
        Reverse(item.candidate.confidence.to_bits()),
        key(item),
    )
}

/// Certainly one root cause: same category, same entity, same words in the title.
fn same_cause(a: &Item, b: &Item) -> bool {
    a.candidate.category == b.candidate.category
        && a.entity == b.entity
        && title_signature(&a.candidate.title) == title_signature(&b.candidate.title)
}

fn place(item: &Item) -> Evidence {
    Evidence {
        kind: EvidenceKind::Source,
        path: item.candidate.path.clone(),
        start_line: item.candidate.start_line,
        end_line: item.candidate.end_line,
    }
}

fn union<'a>(ids: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut ids: Vec<String> = ids.cloned().collect();
    // `e9` before `e10`.
    ids.sort_by_key(|id| (id.len(), id.clone()));
    ids.dedup();
    ids
}

/// Folds `other` into `kept`; whichever is the better report becomes the finding.
fn absorb(kept: &mut Item, other: Item) {
    let (mut best, rest) = if rank(&other) < rank(kept) {
        (other, kept.clone())
    } else {
        (kept.clone(), other)
    };
    let mut also: Vec<Evidence> = best
        .also
        .drain(..)
        .chain(rest.also.iter().cloned())
        .collect();
    also.push(place(&rest));
    let here = place(&best);
    also.retain(|e| *e != here);
    also.sort_by(|a, b| {
        (&a.path, a.start_line, a.end_line).cmp(&(&b.path, b.start_line, b.end_line))
    });
    also.dedup();
    best.also = also;
    // Taken from both, so the judge sees everything either report rested on. The
    // confidence stays the best report's: saying it twice is not more evidence.
    let (a, b) = (&best.candidate, &rest.candidate);
    let evidence = union(a.evidence.iter().chain(&b.evidence));
    let counter = union(a.counterevidence.iter().chain(&b.counterevidence));
    best.candidate.evidence = evidence;
    best.candidate.counterevidence = counter;
    *kept = best;
}

fn overlap(a: &Item, b: &Item) -> bool {
    let (x, y) = (&a.candidate, &b.candidate);
    x.category == y.category
        && x.path == y.path
        && x.side == y.side
        && x.start_line <= y.end_line
        && y.start_line <= x.end_line
}

pub fn run(mut items: Vec<Item>) -> Vec<Item> {
    items.sort_by_cached_key(key);
    let mut kept: Vec<Item> = Vec::new();
    for item in items {
        match kept.iter().position(|k| same_cause(k, &item)) {
            Some(at) => absorb(&mut kept[at], item),
            None => kept.push(item),
        }
    }
    kept.sort_by_cached_key(key);
    // Lines in common don't make one root cause (REVIEW_ENGINE §13): these stay
    // separate, and the judge is told about the neighbour.
    let titles: Vec<String> = kept.iter().map(|i| i.candidate.title.clone()).collect();
    for a in 0..kept.len() {
        let mut near: Vec<String> = (0..kept.len())
            .filter(|b| *b != a && overlap(&kept[a], &kept[*b]))
            .map(|b| titles[b].clone())
            .collect();
        near.sort();
        near.dedup();
        kept[a].overlaps = near;
    }
    kept
}

#[cfg(test)]
mod tests {
    use gitcko_prompts::Category;
    use proptest::prelude::*;

    use super::*;
    use crate::validate::fixtures::item;

    #[test]
    fn one_root_cause_at_two_places_is_one_finding_that_lists_both() {
        let mut a = item("a.rs", "Divides by zero", Category::Correctness, (2, 3));
        let mut b = item("a.rs", "divides  by ZERO", Category::Correctness, (40, 41));
        a.candidate.evidence = vec!["e1".into()];
        b.candidate.evidence = vec!["e2".into()];
        let merged = run(vec![a, b]);
        let [one] = merged.as_slice() else {
            panic!("{}", merged.len())
        };
        assert_eq!(one.also.len(), 1);
        assert_eq!((one.also[0].start_line, one.also[0].end_line), (40, 41));
        assert_eq!(one.candidate.evidence, ["e1", "e2"]);
    }

    #[test]
    fn the_same_words_in_another_entity_are_another_issue() {
        let a = item("a.rs", "Divides by zero", Category::Correctness, (2, 3));
        let mut b = item("a.rs", "Divides by zero", Category::Correctness, (40, 41));
        b.entity = "g".into();
        assert_eq!(run(vec![a, b]).len(), 2);
    }

    #[test]
    fn two_bugs_on_one_line_stay_two_and_the_judge_is_told() {
        let a = item("a.rs", "Divides by zero", Category::Correctness, (2, 3));
        let b = item("a.rs", "Overflows the sum", Category::Correctness, (3, 3));
        let kept = run(vec![a, b]);
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|i| i.also.is_empty()));
        assert_eq!(kept[0].overlaps, ["Overflows the sum"]);
        assert_eq!(kept[1].overlaps, ["Divides by zero"]);
    }

    #[test]
    fn other_categories_files_and_places_are_not_neighbours() {
        let base = item("a.rs", "Divides by zero", Category::Correctness, (2, 3));
        for other in [
            item("a.rs", "Divides by zero", Category::Security, (2, 3)),
            item("b.rs", "Different", Category::Correctness, (2, 3)),
            item("a.rs", "Different", Category::Correctness, (4, 5)),
        ] {
            let kept = run(vec![base.clone(), other]);
            assert_eq!(kept.len(), 2);
            assert!(kept.iter().all(|i| i.overlaps.is_empty()), "{kept:?}");
        }
    }

    #[test]
    fn touching_ranges_overlap_but_adjacent_ones_do_not() {
        let a = item("a.rs", "One", Category::Correctness, (2, 3));
        let touching = item("a.rs", "Two", Category::Correctness, (3, 4));
        assert_eq!(run(vec![a.clone(), touching])[0].overlaps.len(), 1);
        let adjacent = item("a.rs", "Two", Category::Correctness, (4, 5));
        assert!(run(vec![a, adjacent])[0].overlaps.is_empty());
    }

    #[test]
    fn repeating_a_claim_does_not_raise_confidence_or_severity() {
        let mut weak = item("a.rs", "Same", Category::Correctness, (2, 3));
        weak.candidate.confidence = 0.72;
        weak.candidate.severity = Severity::P2;
        let mut strong = item("a.rs", "Same", Category::Correctness, (8, 9));
        strong.candidate.confidence = 0.8;
        strong.candidate.severity = Severity::P1;
        let kept = run(vec![weak.clone(), strong, weak]);
        let [one] = kept.as_slice() else {
            panic!("{}", kept.len())
        };
        // The best claim stands as it was; three reports are not more than the best one.
        assert_eq!(one.candidate.confidence, 0.8);
        assert_eq!(one.candidate.severity, Severity::P1);
        assert_eq!(one.candidate.start_line, 8);
        assert_eq!(one.also.len(), 1);
    }

    #[test]
    fn nothing_to_merge_is_returned_untouched_and_nothing_is_nothing() {
        assert!(run(Vec::new()).is_empty());
        let kept = run(vec![item("a.rs", "Only", Category::Correctness, (1, 1))]);
        assert!(kept[0].also.is_empty() && kept[0].overlaps.is_empty());
    }

    proptest! {
        #[test]
        fn the_result_does_not_depend_on_the_order_candidates_arrived_in(
            specs in proptest::collection::vec((0usize..3, 0usize..3, 1u32..12), 0..8),
            rotate in 0usize..8,
        ) {
            let names = ["Alpha", "Beta", "alpha"];
            let cats = [Category::Correctness, Category::Security, Category::Performance];
            let items: Vec<Item> = specs
                .iter()
                .map(|(n, c, start)| item("a.rs", names[*n], cats[*c], (*start, *start + 2)))
                .collect();
            let mut shuffled = items.clone();
            if !shuffled.is_empty() {
                let by = rotate % shuffled.len();
                shuffled.rotate_left(by);
                shuffled.reverse();
            }
            prop_assert_eq!(format!("{:?}", run(items)), format!("{:?}", run(shuffled)));
        }
    }
}
