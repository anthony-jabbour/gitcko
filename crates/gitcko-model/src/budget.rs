// SPDX-License-Identifier: AGPL-3.0-or-later

//! The run's hard ceilings (MODEL_ROUTING §6). A call reserves its worst case before it
//! is sent and settles with what the provider reported afterwards, so concurrent
//! calls can't jointly overrun. Retries, fallbacks and judge calls spend from the same
//! budget; nothing gives a limit back.

use crate::{Limit, Usage};

/// Tokens estimated from UTF-8 bytes: one per three bytes, rounded up. Deliberately
/// high for code, so a reservation is rarely smaller than the real prompt.
pub fn estimate_tokens(bytes: usize) -> u64 {
    (bytes as u64).div_ceil(3)
}

/// The ceilings for one run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetLimits {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `None` when there's no cost ceiling (local runs). Enforced from provider-reported
    /// cost; when cost is unknown the token ceilings govern.
    pub cost_usd: Option<f64>,
}

/// What the run has used so far, as charged to the budget.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Spent {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

/// One run's budget. Not shared across runs.
#[derive(Debug)]
pub struct RunBudget {
    limits: BudgetLimits,
    spent: Spent,
    reserved_input: u64,
    reserved_output: u64,
}

/// A call's claim on the budget, held from before sending until it settles.
#[must_use = "a reservation holds budget until it is settled"]
#[derive(Debug, PartialEq, Eq)]
pub struct Reservation {
    // Private: `settle` releases exactly what was held, so callers can't change it.
    input_tokens: u64,
    max_output: u64,
}

impl Reservation {
    /// The estimated prompt tokens.
    pub fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    /// The most the call may write; the router sends it as `max_output`.
    pub fn max_output(&self) -> u64 {
        self.max_output
    }
}

impl RunBudget {
    /// A fresh budget with nothing spent.
    pub fn new(limits: BudgetLimits) -> RunBudget {
        RunBudget {
            limits,
            spent: Spent::default(),
            reserved_input: 0,
            reserved_output: 0,
        }
    }

    /// The ceilings this budget was made with.
    pub fn limits(&self) -> &BudgetLimits {
        &self.limits
    }

    /// Reserves a prompt of `prompt_bytes` and up to `wanted_output` tokens of output,
    /// trimmed to what's left. Refuses before anything is sent if the prompt doesn't
    /// fit, no output is left, or the cost ceiling is already reached.
    pub fn reserve(
        &mut self,
        prompt_bytes: usize,
        wanted_output: u64,
    ) -> Result<Reservation, Limit> {
        let free = |limit: u64, spent: u64, reserved: u64| {
            limit.saturating_sub(spent).saturating_sub(reserved)
        };
        let input_tokens = estimate_tokens(prompt_bytes);
        if input_tokens
            > free(
                self.limits.input_tokens,
                self.spent.input_tokens,
                self.reserved_input,
            )
        {
            return Err(Limit::InputTokens);
        }
        let max_output = wanted_output.min(free(
            self.limits.output_tokens,
            self.spent.output_tokens,
            self.reserved_output,
        ));
        if max_output == 0 {
            return Err(Limit::OutputTokens);
        }
        if self
            .limits
            .cost_usd
            .is_some_and(|limit| self.spent.cost_usd >= limit)
        {
            return Err(Limit::Cost);
        }
        self.reserved_input += input_tokens;
        self.reserved_output += max_output;
        Ok(Reservation {
            input_tokens,
            max_output,
        })
    }

    /// Releases the reservation and charges what the provider reported. A count the
    /// provider didn't report is charged at the estimate: the reservation for input,
    /// and the streamed output (`streamed_bytes`) for output.
    pub fn settle(&mut self, reservation: Reservation, reported: &Usage, streamed_bytes: usize) {
        self.reserved_input -= reservation.input_tokens;
        self.reserved_output -= reservation.max_output;
        // Saturating: a provider's numbers are untrusted and may be absurd.
        let input = reported.input_tokens.unwrap_or(reservation.input_tokens);
        let output = reported
            .output_tokens
            .unwrap_or_else(|| estimate_tokens(streamed_bytes));
        self.spent.input_tokens = self.spent.input_tokens.saturating_add(input);
        self.spent.output_tokens = self.spent.output_tokens.saturating_add(output);
        // A negative or non-finite cost would lower or poison the total; only count real spend.
        if let Some(cost) = reported.cost_usd.filter(|c| c.is_finite() && *c > 0.0) {
            self.spent.cost_usd += cost;
        }
    }

    /// What has been charged so far.
    pub fn spent(&self) -> Spent {
        self.spent
    }
}
