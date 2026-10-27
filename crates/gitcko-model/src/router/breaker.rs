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

/// A binding's state and its cycle: the count goes up whenever the breaker opens, so
/// a call admitted before that can't decide what happens next. Nothing is admitted
/// while open, so the trial's permit is the only one of its cycle.
#[derive(Debug, Clone, Copy)]
struct Entry {
    state: State,
    epoch: u64,
}

impl Entry {
    fn set(&mut self, state: State) {
        if matches!(state, State::Open { .. }) {
            self.epoch += 1;
        }
        self.state = state;
    }
}

#[derive(Debug, Default)]
pub(crate) struct Breakers {
    entries: Mutex<BTreeMap<String, Entry>>,
}

/// Leave to make one attempt on a binding. Report its result with [`Permit::finish`];
/// a trial permit dropped without one (the caller gave up) frees the trial slot.
pub(crate) struct Permit<'a> {
    breakers: &'a Breakers,
    binding: String,
    epoch: u64,
    trial: bool,
    finished: bool,
}

impl Breakers {
    /// A permit, or `None` while the binding is benched.
    pub(crate) fn admit(&self, binding: &str) -> Option<Permit<'_>> {
        let mut entries = self.entries.lock().expect("breaker lock");
        let entry = entries.entry(binding.to_owned()).or_insert(Entry {
            state: State::Closed(0),
            epoch: 0,
        });
        let trial = match entry.state {
            State::Closed(_) => false,
            State::Open { until } if Instant::now() >= until => {
                entry.set(State::Trial);
                true
            }
            State::Open { .. } | State::Trial => return None,
        };
        Some(Permit {
            breakers: self,
            binding: binding.to_owned(),
            epoch: entry.epoch,
            trial,
            finished: false,
        })
    }
}

impl Permit<'_> {
    /// Records the attempt. `None` is success. Failures that say nothing about the
    /// binding (the request's own fault, cancellation, the budget) leave the count
    /// as it is. A permit from an earlier cycle changes nothing.
    pub(crate) fn finish(mut self, failure: Option<&ErrorKind>) {
        self.finished = true;
        let mut entries = self.breakers.entries.lock().expect("breaker lock");
        let entry = entries.get_mut(&self.binding).expect("admitted");
        if entry.epoch != self.epoch {
            return;
        }
        let next = match (failure.map(counts), entry.state) {
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
        entry.set(next);
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if self.trial && !self.finished {
            let mut entries = self.breakers.entries.lock().expect("breaker lock");
            if let Some(entry) = entries.get_mut(&self.binding)
                && entry.epoch == self.epoch
            {
                entry.set(State::Open {
                    until: Instant::now(),
                });
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// With overlapping runs, a call admitted before the breaker opened can finish
    /// during the recovery trial. Only the trial decides recovery.
    #[tokio::test(start_paused = true)]
    async fn a_stale_permit_does_not_decide_a_trial() {
        let breakers = Breakers::default();
        let slow_call = breakers.admit("smart").unwrap();
        for _ in 0..THRESHOLD {
            breakers
                .admit("smart")
                .unwrap()
                .finish(Some(&ErrorKind::Server));
        }
        assert!(breakers.admit("smart").is_none(), "open");
        tokio::time::advance(COOLDOWN).await;
        let trial = breakers.admit("smart").unwrap();
        slow_call.finish(None);
        assert!(breakers.admit("smart").is_none(), "still on trial");
        trial.finish(Some(&ErrorKind::Server));
        assert!(breakers.admit("smart").is_none(), "the trial reopened it");
    }
}
