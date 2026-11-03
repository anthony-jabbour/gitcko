// SPDX-License-Identifier: AGPL-3.0-or-later

//! `cargo xtask cassettes record`: runs a suite against a live binding through the
//! recording cassette, on a maintainer's machine only. Refused under `CI`, so no
//! workflow can spend a key or write recordings; the cassette itself refuses to write
//! a file that fails the leak scan.

use std::path::Path;

use gitcko_protocol::review::Review;

use crate::Result;
use crate::eval::{self, Engine, Models, Suite};

/// Stops a suite here even if the key's own limit is higher (P4_PLAN §7.3).
const DEFAULT_CAP: f64 = 25.0;

/// Dollars as a positive number, with or without the `$`.
pub fn parse_usd(text: &str) -> Result<f64> {
    let value: f64 = text
        .trim_start_matches('$')
        .parse()
        .map_err(|_| format!("not an amount: {text}"))?;
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(format!("not an amount: {text}"))
    }
}

#[derive(Debug, PartialEq)]
pub struct RecordArgs {
    pub suite: Suite,
    pub binding: String,
    pub spend_cap: f64,
    pub only: Option<String>,
}

pub fn parse(args: &[&str]) -> Result<RecordArgs> {
    let (mut suite, mut binding, mut spend_cap, mut only) = (None, None, DEFAULT_CAP, None);
    let mut rest = args;
    while let [flag, value, tail @ ..] = rest {
        match *flag {
            "--suite" => suite = Some(Suite::parse(value)?),
            "--model" => binding = Some(value.to_string()),
            "--spend-cap" => spend_cap = parse_usd(value)?,
            "--only" => only = Some(value.to_string()),
            _ => return Err(format!("unknown flag {flag}\n{}", crate::USAGE)),
        }
        rest = tail;
    }
    if !rest.is_empty() {
        return Err(crate::USAGE.to_string());
    }
    Ok(RecordArgs {
        suite: suite.ok_or("cassettes record needs --suite")?,
        binding: binding.ok_or("cassettes record needs --model")?,
        spend_cap,
        only,
    })
}

pub fn run(engine: &dyn Engine, args: &[&str], env: &dyn Fn(&str) -> Option<String>) -> Result {
    if env("CI").is_some() {
        return Err(
            "cassettes record is refused under CI: recordings are made by hand and reviewed".into(),
        );
    }
    let args = parse(args)?;
    let models = Models::Record(args.binding);
    let mut spent = 0.0;
    for case in eval::selected(Path::new(eval::ROOT), args.suite, args.only.as_deref())? {
        if spent >= args.spend_cap {
            return Err(format!(
                "stopped at the spend cap: ${spent:.2} of ${:.2}",
                args.spend_cap
            ));
        }
        let outcome = eval::run_case(engine, &case, &models)?;
        spent += outcome.cost_usd.unwrap_or(0.0);
        ended_as_expected(&case.id(), &case.expect, &outcome.review)?;
        replays(engine, &case)?;

        println!("recorded {} (${spent:.2} so far)", case.id());
    }
    println!("provider-reported spend: ${spent:.2}");
    Ok(())
}

/// A live call that fails is passed through and not recorded, so a run that ended
/// otherwise than its case expects may leave a call replay will ask for and not find.
/// Saying so here lets the case be recorded again while the key is at hand; calls it
/// already holds replay, so only the missing ones go live.
fn ended_as_expected(id: &str, expect: &eval::Expect, review: &Review) -> Result {
    let problems = eval::honesty_problems(expect, review);
    if problems.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{id}: {}; a call may have failed live, so record it again",
        problems.join("; ")
    ))
}

/// Replays the case just recorded, offline. An optional call that failed live (the risk
/// check falls back to rules) leaves no cassette and needn't change how the run ends, so
/// only a replay proves every call is held. A missing one panics in replay by design;
/// that becomes "record it again" while the key is at hand.
fn replays(engine: &dyn Engine, case: &eval::Case) -> Result {
    let replayed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eval::run_case(engine, case, &Models::Replay)
    }));
    match replayed {
        Ok(Ok(outcome)) => ended_as_expected(&case.id(), &case.expect, &outcome.review),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!(
            "{}: a call isn't held for replay, so it may have failed live: record it again",
            case.id()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::NotBuilt;

    #[test]
    fn recording_is_refused_under_ci_before_anything_runs() {
        let args = ["--suite", "A", "--model", "m"];
        let err = run(&NotBuilt, &args, &|n| {
            (n == "CI").then(|| "true".to_string())
        })
        .unwrap_err();
        assert!(err.contains("refused under CI"), "{err}");
        // Control: the same arguments get past the guard without CI.
        let err = run(&NotBuilt, &["--suite", "A"], &|_| None).unwrap_err();
        assert!(err.contains("--model"), "{err}");
    }

    #[test]
    fn a_run_that_ended_otherwise_than_expected_is_not_called_recorded() {
        use crate::eval::tests::{clean_expect, review};
        let mut failed = review();
        failed.status = gitcko_protocol::review::ReviewStatus::Partial;
        failed.reason_codes = vec!["provider_failure".into()];
        let err = ended_as_expected("a/x", &clean_expect(), &failed).unwrap_err();
        assert!(err.contains("record it again"), "{err}");
        // Control: the run the case expects passes.
        assert!(ended_as_expected("a/x", &clean_expect(), &review()).is_ok());
    }

    /// Records fine live; on replay, a call it made isn't held.
    struct LosesACall;

    impl Engine for LosesACall {
        fn review(
            &self,
            _case: &eval::Case,
            _repo: &Path,
            models: &Models,
        ) -> Result<eval::Outcome> {
            if *models == Models::Replay {
                panic!("no cassette for this model call");
            }
            Ok(eval::Outcome {
                review: crate::eval::tests::review(),
                cost_usd: Some(0.0),
            })
        }
    }

    #[test]
    fn a_case_whose_calls_dont_all_replay_is_not_called_recorded() {
        let root = gitcko_test_support::workspace_root().join(eval::ROOT);
        let case = eval::selected(&root, Suite::P, Some("rename_pure"))
            .unwrap()
            .remove(0);
        let err = replays(&LosesACall, &case).unwrap_err();
        assert!(
            err.contains("p/rename_pure") && err.contains("record it again"),
            "{err}"
        );
    }

    #[test]
    fn amounts_are_positive_and_finite() {
        assert_eq!(parse_usd("$15").unwrap(), 15.0);
        assert_eq!(parse_usd("2.5").unwrap(), 2.5);
        for bad in ["0", "-1", "inf", "NaN", "lots"] {
            assert!(parse_usd(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn arguments_default_the_cap_and_name_what_is_missing() {
        let args = parse(&["--suite", "F", "--model", "m"]).unwrap();
        assert_eq!(
            args,
            RecordArgs {
                suite: Suite::F,
                binding: "m".into(),
                spend_cap: DEFAULT_CAP,
                only: None
            }
        );
        assert!(parse(&["--model", "m"]).unwrap_err().contains("--suite"));
        assert!(parse(&["--suite", "F", "--nope", "x"]).is_err());
    }
}
