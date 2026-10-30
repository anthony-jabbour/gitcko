// SPDX-License-Identifier: AGPL-3.0-or-later

//! `LineRange`: the 1-based, inclusive line range a finding or its evidence cites.

use gitcko_domain::{LineRange, LineRangeError, Side};
use proptest::prelude::*;

#[test]
fn a_range_starts_at_line_1_and_never_ends_before_it_starts() {
    assert_eq!(
        LineRange::new(Side::New, 0, 3),
        Err(LineRangeError::ZeroLine)
    );
    assert_eq!(
        LineRange::new(Side::New, 5, 4),
        Err(LineRangeError::EndBeforeStart)
    );
    let one = LineRange::new(Side::Old, 7, 7).unwrap();
    assert_eq!((one.side(), one.start(), one.end()), (Side::Old, 7, 7));
}

#[test]
fn ranges_overlap_only_on_the_same_side() {
    let new = |a, b| LineRange::new(Side::New, a, b).unwrap();
    assert!(new(1, 5).overlaps(&new(5, 9)));
    assert!(!new(1, 4).overlaps(&new(5, 9)));
    assert!(!new(1, 5).overlaps(&LineRange::new(Side::Old, 1, 5).unwrap()));
    assert!(new(3, 6).contains(3) && new(3, 6).contains(6));
    assert!(!new(3, 6).contains(2) && !new(3, 6).contains(7));
}

proptest! {
    #[test]
    fn overlap_is_symmetric_and_matches_a_shared_line(
        a in 1u32..50, la in 0u32..10, b in 1u32..50, lb in 0u32..10,
    ) {
        let x = LineRange::new(Side::New, a, a + la).unwrap();
        let y = LineRange::new(Side::New, b, b + lb).unwrap();
        let shared = (a..=a + la).any(|line| y.contains(line));
        prop_assert_eq!(x.overlaps(&y), shared);
        prop_assert_eq!(x.overlaps(&y), y.overlaps(&x));
    }
}
