// SPDX-License-Identifier: AGPL-3.0-or-later

//! Hostile model replies (TM-06): each corpus file in `tests/fixtures/outputs/` is
//! accepted or refused with a typed, value-free problem, never a panic.

use std::path::PathBuf;

use gitcko_prompts::{
    Checked, InvestigationOutput, JudgeOutput, MAX_REPLY, OutputError, PlanOutput, Problem, parse,
};
use proptest::prelude::*;

fn corpus(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/outputs")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

fn problems<T: Checked>(name: &str) -> Result<(), Vec<Problem>> {
    parse::<T>(&corpus(name))
        .map(|_| ())
        .map_err(|e: OutputError| e.problems)
}

#[test]
fn every_corpus_file_has_its_expected_result() {
    use Problem::*;
    let investigation: &[(&str, Result<(), Vec<Problem>>)] = &[
        ("valid.txt", Ok(())),
        ("fenced.txt", Ok(())),
        // Instructions inside a field are just text to the parser; the judge and the
        // fence handle what they mean.
        ("injected_reason.txt", Ok(())),
        ("prose_around.txt", Err(vec![NotJson])),
        ("truncated.txt", Err(vec![NotJson])),
        ("two_fences.txt", Err(vec![NotJson])),
        ("nan_confidence.txt", Err(vec![NotJson])),
        ("unknown_field.txt", Err(vec![Schema])),
        ("wrong_enum.txt", Err(vec![Schema])),
        ("escaping_path.txt", Err(vec![Schema])),
        (
            "confidence_above_one.txt",
            Err(vec![BadNumber("confidence")]),
        ),
        (
            "confidence_negative.txt",
            Err(vec![BadNumber("confidence")]),
        ),
        ("zero_line.txt", Err(vec![BadLines("start_line")])),
        ("reversed_lines.txt", Err(vec![BadLines("start_line")])),
        ("bad_evidence_id.txt", Err(vec![BadEvidence("evidence")])),
        ("short_evidence_id.txt", Err(vec![BadEvidence("evidence")])),
        ("array.txt", Err(vec![NotJson])),
        ("no_evidence.txt", Err(vec![Empty("evidence")])),
        (
            "supported_without_candidates.txt",
            Err(vec![Missing("candidates")]),
        ),
        ("too_many_candidates.txt", Err(vec![TooMany("candidates")])),
    ];
    for (name, expected) in investigation {
        assert_eq!(&problems::<InvestigationOutput>(name), expected, "{name}");
    }
    assert_eq!(
        problems::<JudgeOutput>("judge_follow_up_without_fact.txt"),
        Err(vec![Missing("missing_fact")])
    );
    assert_eq!(
        problems::<PlanOutput>("plan_nine_hypotheses.txt"),
        Err(vec![TooMany("hypotheses")])
    );
    // Every file in the folder is in a table above.
    let listed = investigation.len() + 2;
    let files = std::fs::read_dir(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/outputs"),
    )
    .unwrap()
    .filter(|e| {
        e.as_ref()
            .unwrap()
            .path()
            .extension()
            .is_some_and(|x| x == "txt")
    })
    .count();
    assert_eq!(files, listed);
}

#[test]
fn huge_replies_are_refused_unread() {
    let valid = corpus("valid.txt");
    let padded = format!("{valid}{}", " ".repeat(MAX_REPLY));
    assert_eq!(
        parse::<InvestigationOutput>(&padded).unwrap_err().problems,
        [Problem::TooLarge]
    );
    // Many evidence ids within the byte cap still hit the item cap.
    let ids: Vec<String> = (1..=5000).map(|i| format!("\"e{i}\"")).collect();
    let many = valid.replace("[\"e1\", \"e2\"]", &format!("[{}]", ids.join(",")));
    assert_ne!(many, valid, "the fixture's evidence list was found");
    let err = parse::<InvestigationOutput>(&many).unwrap_err();
    assert!(
        err.problems.contains(&Problem::TooMany("evidence")),
        "{err:?}"
    );
}

#[test]
fn canary_problems_never_carry_model_text() {
    let valid = corpus("valid.txt");
    for canary in gitcko_redact::canaries() {
        let escaped = serde_json::to_string(&canary).unwrap();
        for bad in [
            // As a path that escapes the repository.
            valid.replace(
                "\"src/lib.rs\"",
                &serde_json::to_string(&format!("../{canary}")).unwrap(),
            ),
            valid.replace("\"P1\"", &escaped),
            format!("{canary}{valid}"),
            valid.replace("\"e1\"", &escaped),
        ] {
            let err = parse::<InvestigationOutput>(&bad).unwrap_err();
            let shown = format!(
                "{err} {err:?} {}",
                err.problems
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<String>()
            );
            assert!(!shown.contains(&canary), "{shown}");
        }
    }
}

#[test]
fn each_problem_is_listed_once_however_many_candidates_share_it() {
    let mut value: serde_json::Value = serde_json::from_str(&corpus("valid.txt")).unwrap();
    let candidate = &mut value["candidates"][0];
    candidate["confidence"] = 2.0.into();
    candidate["start_line"] = 0.into();
    let bad = candidate.clone();
    value["candidates"] = serde_json::json!([bad, bad]);
    let err = parse::<InvestigationOutput>(&value.to_string()).unwrap_err();
    assert_eq!(
        err.problems,
        [
            Problem::BadNumber("confidence"),
            Problem::BadLines("start_line")
        ]
    );
}

proptest! {
    #[test]
    fn key_order_and_whitespace_never_change_the_result(seed in any::<u64>(), pad in "[ \n\t]{0,4}") {
        // Reorder the top-level keys and re-space the JSON; the parse must not change.
        let value: serde_json::Value = serde_json::from_str(&corpus("valid.txt")).unwrap();
        let mut pairs: Vec<(String, serde_json::Value)> = value.as_object().unwrap().clone().into_iter().collect();
        let n = pairs.len();
        pairs.rotate_left((seed as usize) % n);
        let body: Vec<String> = pairs.iter().map(|(k, v)| format!("{pad}\"{k}\"{pad}:{pad}{v}")).collect();
        let text = format!("{pad}{{{}}}{pad}", body.join(","));
        let a = parse::<InvestigationOutput>(&text).unwrap();
        let b = parse::<InvestigationOutput>(&corpus("valid.txt")).unwrap();
        prop_assert_eq!(a, b);
    }

    #[test]
    fn any_text_parses_or_fails_typed(text in ".{0,300}") {
        let _ = parse::<InvestigationOutput>(&text);
        let _ = parse::<JudgeOutput>(&text);
        let _ = parse::<PlanOutput>(&text);
    }
}
