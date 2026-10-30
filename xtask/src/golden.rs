// SPDX-License-Identifier: AGPL-3.0-or-later

//! `cargo xtask golden`: every review case on cassettes, compared with its committed
//! result in `tests/goldens/`. `--bless` rewrites them for review in the diff.

use std::path::Path;

use gitcko_protocol::review::Review;
use serde_json::{Value, json};

use crate::Result;
use crate::eval::{self, Case, Engine, Models, Outcome, Suite};

const GOLDENS: &str = "tests/goldens";

/// The result with what legitimately differs between runs and releases replaced: ids,
/// versions, usage and event timings. Findings keep everything a reader acts on.
pub fn normalise(review: &Review) -> Result<Value> {
    let mut value = serde_json::to_value(review).map_err(|e| format!("result as JSON: {e}"))?;
    let root = value.as_object_mut().ok_or("a review is an object")?;
    for key in ["review_id", "versions", "usage", "events"] {
        if root.contains_key(key) {
            root.insert(key.into(), json!("<normalised>"));
        }
    }
    Ok(value)
}

/// The golden's text: pretty JSON with a trailing newline, so diffs read well.
fn render(value: &Value) -> Result<String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| format!("golden as JSON: {e}"))?;
    Ok(format!("{text}\n"))
}

/// Compares one case's run with its golden, or rewrites it. `Err` says what differs.
pub fn check_case(goldens: &Path, case: &Case, outcome: &Outcome, bless: bool) -> Result {
    let path = goldens
        .join(case.suite.folder())
        .join(format!("{}.json", case.name));
    let got = render(&normalise(&outcome.review)?)?;
    if bless {
        let parent = path.parent().ok_or("a golden has a folder")?;
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        return std::fs::write(&path, got).map_err(|e| format!("{}: {e}", path.display()));
    }
    let want = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "{}: no golden; run `cargo xtask golden --bless` and review it",
            case.id()
        )
    })?;
    if got != want {
        return Err(format!("{}: differs from {}", case.id(), path.display()));
    }
    Ok(())
}

pub fn run(engine: &dyn Engine, bless: bool) -> Result {
    let mut failures = Vec::new();
    let mut checked = 0;
    for suite in Suite::ALL {
        for case in eval::load(Path::new(eval::ROOT), suite)? {
            let outcome = eval::run_case(engine, &case, &Models::Replay)?;
            let score = eval::score(&case, &outcome);
            failures.extend(score.problems.iter().map(|p| format!("{}: {p}", case.id())));
            if let Err(e) = check_case(Path::new(GOLDENS), &case, &outcome, bless) {
                failures.push(e);
            }
            checked += 1;
        }
    }
    if failures.is_empty() {
        println!("golden: {checked} cases ok");
        Ok(())
    } else {
        Err(format!("golden:\n  {}", failures.join("\n  ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::tests::review;
    use crate::eval::{Expect, Kind};
    use gitcko_protocol::review::ReviewStatus;

    fn case() -> Case {
        Case {
            suite: Suite::P,
            name: "c".into(),
            dir: Default::default(),
            expect: Expect {
                kind: Kind::Clean,
                status: ReviewStatus::Complete,
                reason_codes: Vec::new(),
                findings: Vec::new(),
                silences: Vec::new(),
                fault: None,
            },
        }
    }

    fn outcome(review: Review) -> Outcome {
        Outcome {
            review,
            cost_usd: None,
        }
    }

    #[test]
    fn run_details_are_normalised_and_results_are_not() {
        let a = review();
        let mut b = review();
        b.review_id = "another".into();
        b.versions.engine = "9.9.9".into();
        assert_eq!(normalise(&a).unwrap(), normalise(&b).unwrap());
        let mut c = review();
        c.summary = Some("different".into());
        assert_ne!(normalise(&a).unwrap(), normalise(&c).unwrap());
        c = review();
        c.status = ReviewStatus::Partial;
        assert_ne!(normalise(&a).unwrap(), normalise(&c).unwrap());
    }

    #[test]
    fn a_golden_is_blessed_then_compared_and_a_change_fails() {
        let dir = gitcko_test_support::TempDir::new("golden");
        let same = outcome(review());
        let err = check_case(dir.path(), &case(), &same, false).unwrap_err();
        assert!(err.contains("no golden"), "{err}");
        check_case(dir.path(), &case(), &same, true).unwrap();
        check_case(dir.path(), &case(), &same, false).unwrap();
        let mut changed = review();
        changed.summary = Some("different".into());
        let err = check_case(dir.path(), &case(), &outcome(changed.clone()), false).unwrap_err();
        assert!(err.contains("differs"), "{err}");
        check_case(dir.path(), &case(), &outcome(changed.clone()), true).unwrap();
        check_case(dir.path(), &case(), &outcome(changed), false).unwrap();
    }

    #[test]
    fn a_golden_ends_in_one_newline_and_holds_no_run_id() {
        let text = render(&normalise(&review()).unwrap()).unwrap();
        assert!(text.ends_with("}\n") && !text.ends_with("\n\n"));
        assert!(!text.contains(&review().review_id));
    }
}
