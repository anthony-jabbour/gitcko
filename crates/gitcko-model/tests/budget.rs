// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reserve before sending, settle after: refusals, unknown usage, and the ceiling
//! property over random call sequences (MODEL_ROUTING §6).

use gitcko_model::{BudgetLimits, Limit, RunBudget, Usage, estimate_tokens};
use proptest::prelude::*;

fn budget(input: u64, output: u64, cost: Option<f64>) -> RunBudget {
    RunBudget::new(BudgetLimits {
        input_tokens: input,
        output_tokens: output,
        cost_usd: cost,
    })
}

fn reported(input: u64, output: u64, cost: Option<f64>) -> Usage {
    Usage {
        input_tokens: Some(input),
        output_tokens: Some(output),
        cost_usd: cost,
    }
}

#[test]
fn tokens_are_estimated_at_a_third_of_the_bytes_rounded_up() {
    assert_eq!(estimate_tokens(0), 0);
    assert_eq!(estimate_tokens(1), 1);
    assert_eq!(estimate_tokens(3), 1);
    assert_eq!(estimate_tokens(4), 2);
    assert_eq!(estimate_tokens(300), 100);
}

#[test]
fn a_prompt_that_does_not_fit_is_refused_before_sending() {
    let mut b = budget(100, 50, None);
    assert_eq!(b.reserve(301, 10), Err(Limit::InputTokens));
    let r = b.reserve(300, 10).unwrap();
    assert_eq!((r.input_tokens, r.max_output), (100, 10));
    // The first reservation holds all the input until it settles.
    assert_eq!(b.reserve(1, 10), Err(Limit::InputTokens));
    b.settle(r, &reported(40, 5, None), 0);
    assert_eq!(b.reserve(180, 10).map(|r| r.input_tokens), Ok(60));
}

#[test]
fn output_is_trimmed_to_what_is_left_and_refused_at_zero() {
    let mut b = budget(1000, 50, None);
    let first = b.reserve(3, 40).unwrap();
    assert_eq!(first.max_output, 40);
    let second = b.reserve(3, 40).unwrap();
    assert_eq!(second.max_output, 10, "trimmed to the 10 not yet reserved");
    assert_eq!(b.reserve(3, 40), Err(Limit::OutputTokens));
    b.settle(first, &reported(1, 40, None), 0);
    b.settle(second, &reported(1, 10, None), 0);
    assert_eq!(b.reserve(3, 1), Err(Limit::OutputTokens));
}

#[test]
fn unknown_usage_is_charged_at_the_estimate() {
    let mut b = budget(1000, 1000, None);
    let r = b.reserve(30, 500).unwrap();
    // Input unknown: the reservation's estimate. Output unknown: the streamed bytes.
    b.settle(r, &Usage::default(), 61);
    let spent = b.spent();
    assert_eq!((spent.input_tokens, spent.output_tokens), (10, 21));
    assert_eq!(
        spent.cost_usd, 0.0,
        "unknown cost adds nothing; tokens govern"
    );
}

#[test]
fn the_cost_ceiling_stops_new_calls_once_reported_cost_reaches_it() {
    let mut b = budget(1000, 1000, Some(0.25));
    let r = b.reserve(3, 10).unwrap();
    b.settle(r, &reported(1, 10, Some(0.20)), 0);
    let r = b.reserve(3, 10).unwrap();
    b.settle(r, &reported(1, 10, Some(0.05)), 0);
    assert_eq!(b.reserve(3, 10), Err(Limit::Cost));
    // No cost ceiling: cost is recorded but never refuses.
    let mut b = budget(1000, 1000, None);
    let r = b.reserve(3, 10).unwrap();
    b.settle(r, &reported(1, 10, Some(9.0)), 0);
    assert!(b.reserve(3, 10).is_ok());
}

#[test]
fn a_provider_reporting_more_than_reserved_exhausts_the_budget_instead_of_wrapping() {
    let mut b = budget(100, 100, None);
    let r = b.reserve(30, 10).unwrap();
    b.settle(r, &reported(150, 10, None), 0);
    assert_eq!(b.spent().input_tokens, 150, "the truth is recorded");
    assert_eq!(b.reserve(1, 1), Err(Limit::InputTokens));
}

#[test]
fn absurd_reported_usage_saturates_and_never_lowers_spend() {
    // Two calls in flight, both reporting u64::MAX: the second charge would overflow.
    let mut b = budget(100, 100, None);
    let first = b.reserve(3, 10).unwrap();
    let second = b.reserve(3, 10).unwrap();
    b.settle(first, &reported(u64::MAX, u64::MAX, None), 0);
    b.settle(second, &reported(u64::MAX, u64::MAX, None), 0);
    assert_eq!(b.spent().input_tokens, u64::MAX);
    assert_eq!(b.reserve(0, 1), Err(Limit::OutputTokens));
    // A negative or non-finite cost is ignored rather than lowering or poisoning spend.
    for cost in [-5.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut b = budget(100, 100, Some(1.0));
        let r = b.reserve(3, 1).unwrap();
        b.settle(r, &reported(1, 1, Some(cost)), 0);
        assert_eq!(b.spent().cost_usd, 0.0, "{cost}");
    }
}

#[derive(Debug, Clone)]
enum Step {
    Reserve {
        bytes: usize,
        wanted: u64,
    },
    /// Settle the outstanding reservation at this index (mod the count), reporting
    /// these fractions of what it reserved, or unknown.
    Settle {
        pick: usize,
        input_pct: Option<u64>,
        output_pct: Option<u64>,
        streamed: usize,
    },
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0usize..3000, 0u64..400).prop_map(|(bytes, wanted)| Step::Reserve { bytes, wanted }),
        (
            any::<usize>(),
            proptest::option::of(0u64..=100),
            proptest::option::of(0u64..=100),
            0usize..1200
        )
            .prop_map(|(pick, input_pct, output_pct, streamed)| Step::Settle {
                pick,
                input_pct,
                output_pct,
                streamed
            }),
    ]
}

proptest! {
    /// Whatever the order of calls, as long as providers report no more than was
    /// reserved and streamed output stays inside `max_output` (the router cancels at
    /// it), spent plus reserved never passes a ceiling.
    #[test]
    fn spent_never_passes_a_ceiling(
        input in 0u64..2000,
        output in 0u64..1000,
        steps in proptest::collection::vec(step(), 0..60),
    ) {
        let mut b = budget(input, output, None);
        let mut open = Vec::new();
        for s in steps {
            match s {
                Step::Reserve { bytes, wanted } => {
                    if let Ok(r) = b.reserve(bytes, wanted) {
                        prop_assert!(r.max_output <= wanted);
                        open.push(r);
                    }
                }
                Step::Settle { pick, input_pct, output_pct, streamed } if !open.is_empty() => {
                    let r = open.swap_remove(pick % open.len());
                    let usage = Usage {
                        input_tokens: input_pct.map(|p| r.input_tokens * p / 100),
                        output_tokens: output_pct.map(|p| r.max_output * p / 100),
                        cost_usd: None,
                    };
                    let streamed = streamed.min(usize::try_from(r.max_output * 3).unwrap());
                    b.settle(r, &usage, streamed);
                }
                Step::Settle { .. } => {}
            }
            let held_in: u64 = open.iter().map(|r| r.input_tokens).sum();
            let held_out: u64 = open.iter().map(|r| r.max_output).sum();
            prop_assert!(b.spent().input_tokens + held_in <= input);
            prop_assert!(b.spent().output_tokens + held_out <= output);
        }
    }
}
