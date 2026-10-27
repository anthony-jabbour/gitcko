// SPDX-License-Identifier: AGPL-3.0-or-later

//! When a failed attempt is tried again, after how long, and how long an attempt may
//! take (MODEL_ROUTING §7).

use std::time::Duration;

use gitcko_protocol::config::ModelRole;
use sha2::{Digest, Sha256};

use crate::{DestinationClass, ErrorKind};

/// How long an attempt may wait for its first event and run in total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub first_token: Duration,
    pub total: Duration,
}

/// MODEL_ROUTING §7. Local models are slower, so their limits are longer; background
/// roles get 300 s in total. Every call streams, so the non-streaming limits (120 s,
/// 300 s) have no use here.
pub fn timeouts(destination: DestinationClass, role: ModelRole) -> Timeouts {
    let secs = |first, total| Timeouts {
        first_token: Duration::from_secs(first),
        total: Duration::from_secs(total),
    };
    let background = matches!(
        role,
        ModelRole::MemoryLearner | ModelRole::AddressedJudge | ModelRole::Digest
    );
    match destination {
        DestinationClass::OnDevice => secs(120, 900),
        DestinationClass::CustomerRemote if background => secs(30, 300),
        DestinationClass::CustomerRemote => secs(30, 600),
    }
}

/// Retries after the first attempt: remote 2, local 1.
pub fn retries(destination: DestinationClass) -> u32 {
    match destination {
        DestinationClass::OnDevice => 1,
        DestinationClass::CustomerRemote => 2,
    }
}

/// Failures that may pass on their own: timeouts, 429, 5xx and dropped connections.
/// Adapters map OpenRouter's in-flight-budget 402 to `RateLimited` and a plain 402
/// to `InsufficientCredits`, which is not retried. Auth, validation, content policy
/// and context length never succeed on a second try.
pub fn retryable(kind: &ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::RateLimited { .. }
            | ErrorKind::Overloaded
            | ErrorKind::Server
            | ErrorKind::Timeout(_)
            | ErrorKind::Connect
    )
}

/// Exponential backoff with full jitter: a delay in `[0, 1 s × 2^(retry-1)]`.
/// The "random" part is a hash of the run, binding and retry number, so a run's
/// delays are the same every time it is replayed. Full jitter: AWS Architecture Blog,
/// "Exponential Backoff And Jitter" (2015).
pub fn backoff(run_id: &str, binding: &str, retry: u32) -> Duration {
    let cap_ms = 1_000u64 << retry.saturating_sub(1).min(6);
    let digest = Sha256::digest(format!("{run_id}\0{binding}\0{retry}"));
    let draw = u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"));
    Duration::from_millis(draw % (cap_ms + 1))
}
