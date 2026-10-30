// SPDX-License-Identifier: AGPL-3.0-or-later

//! `cargo xtask cassettes record`: runs a suite against a live binding through the
//! recording cassette, on a maintainer's machine only. Refused under `CI`, so no
//! workflow can spend a key or write recordings; the cassette itself refuses to write
//! a file that fails the leak scan.

use std::path::Path;

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
        println!("recorded {} (${spent:.2} so far)", case.id());
    }
    println!("provider-reported spend: ${spent:.2}");
    Ok(())
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
