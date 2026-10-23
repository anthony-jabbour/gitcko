// SPDX-License-Identifier: AGPL-3.0-or-later

use gitcko_domain::{ContentHash, Coverage, Position, Reason, SourceSpan};
use proptest::prelude::*;

fn reason(code: &str) -> Reason {
    Reason::new(code).unwrap()
}

#[test]
fn complete_and_partial_zero_results_stay_distinguishable() {
    // INDEXING §27.6: "complete + zero results" must never look like "partial + zero".
    let complete = Coverage::Complete {};
    let partial = Coverage::Partial {
        reason: reason("parse_timeout"),
        omitted: None,
    };
    assert!(complete.is_complete());
    assert!(!partial.is_complete());
    assert!(
        !Coverage::Unknown {
            reason: reason("dynamic_dispatch")
        }
        .is_complete()
    );
    assert!(
        !Coverage::Unavailable {
            reason: reason("embeddings_disabled")
        }
        .is_complete()
    );

    let json = |c: &Coverage| serde_json::to_string(c).unwrap();
    assert_eq!(json(&complete), r#"{"state":"complete"}"#);
    assert_eq!(
        json(&partial),
        r#"{"state":"partial","reason":"parse_timeout","omitted":null}"#
    );
    for c in [complete, partial] {
        assert_eq!(serde_json::from_str::<Coverage>(&json(&c)).unwrap(), c);
    }
}

#[test]
fn coverage_rejects_impossible_shapes_on_read() {
    for bad in [
        r#"{"state":"partial"}"#,                        // no reason
        r#"{"state":"complete","omitted":3}"#,           // complete can't omit
        r#"{"state":"unknown","reason":"src/main.rs"}"#, // a path isn't a reason
        r#"{"state":"maybe"}"#,
    ] {
        assert!(serde_json::from_str::<Coverage>(bad).is_err(), "{bad}");
    }
}

#[test]
fn reasons_are_short_codes_that_cannot_carry_repository_text() {
    for good in ["a", "parse_timeout", "limit_2", &"x".repeat(64)] {
        assert_eq!(Reason::new(good).unwrap().as_str(), good);
    }
    // Each bad value breaks exactly one rule, so every rule is tested on its own.
    for bad in [
        "",
        "Parse",
        "src/main",
        "main.rs",
        "with space",
        "dash-ed",
        "naïve",
        &"x".repeat(65),
    ] {
        assert!(Reason::new(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn canary_reason_error_never_shows_the_rejected_value() {
    for canary in gitcko_redact::canaries() {
        let err = Reason::new(&canary).unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains(&canary), "{shown}");
    }
}

#[test]
fn canary_hash_and_span_errors_never_show_input() {
    for canary in gitcko_redact::canaries() {
        let err = canary.parse::<ContentHash>().unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains(&canary), "{shown}");
    }
    let p = |row| Position { row, column: 0 };
    let err = SourceSpan::new(9, 1, p(0), p(0)).unwrap_err();
    assert!(!format!("{err} {err:?}").contains('9'));
}

#[test]
fn content_hash_matches_sha256_test_vectors() {
    // FIPS 180-2 examples.
    assert_eq!(
        ContentHash::of(b"").to_string(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        ContentHash::of(b"abc").to_string(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn content_hash_text_round_trips_and_rejects_other_spellings() {
    let hash = ContentHash::of(b"fn main() {}\n");
    let text = hash.to_string();
    assert_eq!(text.parse::<ContentHash>().unwrap(), hash);
    let json = serde_json::to_string(&hash).unwrap();
    assert_eq!(serde_json::from_str::<ContentHash>(&json).unwrap(), hash);
    for bad in [
        String::new(),
        text[..63].to_string(),
        format!("{text}0"),
        text.to_uppercase(),
        format!("g{}", &text[1..]),
    ] {
        assert!(bad.parse::<ContentHash>().is_err(), "{bad}");
    }
}

#[test]
fn spans_never_end_before_they_start() {
    let p = |row, column| Position { row, column };
    assert!(SourceSpan::new(0, 0, p(0, 0), p(0, 0)).is_ok());
    assert!(SourceSpan::new(5, 4, p(0, 5), p(0, 6)).is_err());
    assert!(SourceSpan::new(4, 5, p(1, 0), p(0, 9)).is_err());
    let raw =
        r#"{"start_byte":5,"end_byte":4,"start":{"row":0,"column":5},"end":{"row":0,"column":6}}"#;
    assert!(serde_json::from_str::<SourceSpan>(raw).is_err());
}

#[test]
fn span_lines_are_one_based() {
    let p = |row, column| Position { row, column };
    let span = SourceSpan::new(10, 30, p(2, 4), p(5, 1)).unwrap();
    assert_eq!(span.lines(), (3, 6));
}

proptest! {
    #[test]
    fn valid_spans_round_trip_through_json(
        a in 0usize..10_000, len in 0usize..10_000,
        r1 in 0usize..500, rows in 0usize..500, c1 in 0usize..200, c2 in 0usize..200,
    ) {
        let start = Position { row: r1, column: c1 };
        let end = Position { row: r1 + rows, column: if rows == 0 { c1 + c2 } else { c2 } };
        let span = SourceSpan::new(a, a + len, start, end).unwrap();
        let back: SourceSpan = serde_json::from_str(&serde_json::to_string(&span).unwrap()).unwrap();
        prop_assert_eq!(back, span);
    }
}
