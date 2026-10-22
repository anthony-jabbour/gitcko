// SPDX-License-Identifier: AGPL-3.0-or-later

use gitcko_domain::{Band, Confidence, FindingId, QuestionId, ReviewId};
use proptest::prelude::*;

#[test]
fn confidence_rejects_values_that_are_not_a_probability() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 1.01] {
        assert!(
            Confidence::try_from(bad).is_err(),
            "{bad} should be rejected"
        );
    }
    for good in [0.0, 0.5, 1.0] {
        assert_eq!(Confidence::try_from(good).unwrap().value(), good);
    }
}

#[test]
fn confidence_bands_use_the_unrounded_value() {
    // REVIEW_ENGINE §15: High >= 0.85, Medium >= 0.70, otherwise Low, on the exact number.
    let band = |c: f64| Confidence::try_from(c).unwrap().band();
    assert_eq!(band(0.85), Band::High);
    assert_eq!(band(0.849_999), Band::Medium);
    assert_eq!(band(0.70), Band::Medium);
    assert_eq!(band(0.699_999), Band::Low);
    assert_eq!(band(1.0), Band::High);
    assert_eq!(band(0.0), Band::Low);
}

#[test]
fn review_id_looks_like_the_documented_format() {
    let id = ReviewId::new().to_string();
    assert_eq!(id.len(), 3 + 26, "{id}");
    assert!(id.starts_with("rv_"), "{id}");
    assert_eq!(id.parse::<ReviewId>().unwrap().to_string(), id);
}

#[test]
fn review_ids_are_unique() {
    let ids: std::collections::HashSet<_> =
        (0..1000).map(|_| ReviewId::new().to_string()).collect();
    assert_eq!(ids.len(), 1000);
}

#[test]
fn malformed_ids_are_rejected() {
    for bad in [
        "",
        "rv_",
        "rv_01J9ZQ3V7K2M4N6P8R0S2T4V6",   // 25 chars
        "rv_01J9ZQ3V7K2M4N6P8R0S2T4V6XX", // 27 chars
        "rv_01J9ZQ3V7K2M4N6P8R0S2T4V6U",  // U isn't in Crockford base32
        "rv_81J9ZQ3V7K2M4N6P8R0S2T4V6X",  // first char > 7 overflows 128 bits
        "f_01J9ZQ3V7K2M4N6P8R0S2T4V6X",
    ] {
        assert!(bad.parse::<ReviewId>().is_err(), "{bad} should be rejected");
    }
    for bad in [
        "f_",
        "f_0123456789abcde",
        "f_0123456789ABCDEF",
        "q_0123456789abcdef",
        "f_0123456789abcdefg",
    ] {
        assert!(
            bad.parse::<FindingId>().is_err(),
            "{bad} should be rejected"
        );
    }
    assert!("f_0123456789abcdef".parse::<QuestionId>().is_err());
}

#[test]
fn documented_example_ids_parse() {
    "rv_01J9ZQ3V7K2M4N6P8R0S2T4V6X".parse::<ReviewId>().unwrap();
    "f_0123456789abcdef".parse::<FindingId>().unwrap();
    "q_0123456789abcdef".parse::<QuestionId>().unwrap();
}

proptest! {
    #[test]
    fn finding_ids_round_trip(fingerprint: u64) {
        let id = FindingId::from_fingerprint(fingerprint);
        prop_assert_eq!(id.to_string().parse::<FindingId>().unwrap(), id);
    }

    #[test]
    fn question_ids_round_trip(fingerprint: u64) {
        let id = QuestionId::from_fingerprint(fingerprint);
        prop_assert_eq!(id.to_string().parse::<QuestionId>().unwrap(), id);
    }
}
