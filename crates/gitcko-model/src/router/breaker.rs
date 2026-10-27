// SPDX-License-Identifier: AGPL-3.0-or-later

//! A circuit breaker per binding, in this process: 3 consecutive failures bench a
//! binding for 60 s, then one trial call decides (MODEL_ROUTING §7).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

use crate::ErrorKind;

const THRESHOLD: u32 = 3;
const COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
enum State {
    /// Consecutive failures so far.
    Closed(u32),
    Open {
        until: Instant,
    },
    /// One trial call is in flight; everyone else waits for its result.
    Trial,
}

#[derive(Debug, Default)]
pub(crate) struct Breakers {
    states: Mutex<BTreeMap<String, State>>,
}

/// Leave to make one attempt on a binding. Report its result with [`Permit::finish`];
/// a permit dropped without one (the caller gave up) frees the trial slot.
pub(crate) struct Permit<'a> {
    breakers: &'a Breakers,
    binding: String,
    trial: bool,
    finished: bool,
}

impl Breakers {
    /// A permit, or `None` while the binding is benched.
    pub(crate) fn admit(&self, binding: &str) -> Option<Permit<'_>> {
        let mut states = self.states.lock().expect("breaker lock");
        let state = states.entry(binding.to_owned()).or_insert(State::Closed(0));
        let trial = match *state {
            State::Closed(_) => false,
            State::Open { until } if Instant::now() >= until => {
                *state = State::Trial;
                true
            }
            State::Open { .. } | State::Trial => return None,
        };
        Some(Permit {
            breakers: self,
            binding: binding.to_owned(),
            trial,
            finished: false,
        })
    }
}

impl Permit<'_> {
    /// Records the attempt. `None` is success. Failures that say nothing about the
    /// binding (the request's own fault, cancellation, the budget) leave the count
    /// as it is.
    pub(crate) fn finish(mut self, failure: Option<&ErrorKind>) {
        self.finished = true;
        let mut states = self.breakers.states.lock().expect("breaker lock");
        let state = states.get_mut(&self.binding).expect("admitted");
        *state = match (failure.map(counts), *state) {
            (None, _) => State::Closed(0),
            (Some(true), State::Trial) => open(),
            (Some(true), State::Closed(n)) if n + 1 >= THRESHOLD => open(),
            (Some(true), State::Closed(n)) => State::Closed(n + 1),
            // A trial that proved nothing: let the next caller try at once.
            (Some(false), State::Trial) => State::Open {
                until: Instant::now(),
            },
            (Some(_), other) => other,
        };
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if self.trial && !self.finished {
            let mut states = self.breakers.states.lock().expect("breaker lock");
            states.insert(
                self.binding.clone(),
                State::Open {
                    until: Instant::now(),
                },
            );
        }
    }
}

fn open() -> State {
    State::Open {
        until: Instant::now() + COOLDOWN,
    }
}

/// Whether a failure says the binding itself is unhealthy.
fn counts(kind: &ErrorKind) -> bool {
    !matches!(
        kind,
        ErrorKind::BadRequest
            | ErrorKind::ContextLength
            | ErrorKind::ContentPolicy
            | ErrorKind::Canceled
            | ErrorKind::BudgetExhausted(_)
            | ErrorKind::LocalityViolation
    )
}
