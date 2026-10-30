// SPDX-License-Identifier: AGPL-3.0-or-later

//! A model's reply as any bytes, read as every output type. Parsing must return a value
//! or at least one problem, never panic, and give the same answer twice.

#![no_main]

use gitcko_prompts::{
    Checked, InvestigationOutput, JudgeOutput, PlanOutput, RiskOutput, parse,
};
use libfuzzer_sys::fuzz_target;

fn check<T: Checked + PartialEq + std::fmt::Debug>(reply: &str) {
    let first = parse::<T>(reply);
    if let Err(e) = &first {
        assert!(!e.problems.is_empty());
    }
    assert_eq!(first, parse::<T>(reply));
}

fuzz_target!(|data: &[u8]| {
    let reply = String::from_utf8_lossy(data);
    check::<RiskOutput>(&reply);
    check::<PlanOutput>(&reply);
    check::<InvestigationOutput>(&reply);
    check::<JudgeOutput>(&reply);
});
