// SPDX-License-Identifier: AGPL-3.0-or-later

//! Errors carry a kind and at most an HTTP status. Provider bodies, headers, prompts
//! and keys are untrusted or secret, so no variant has room for them: an error can be
//! logged, shown or put in a run record as it is.

use std::time::Duration;

use gitcko_protocol::config::ModelRole;

/// A failed model call. The router decides retry and fallback from `kind` alone.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind}{}", status.map(|s| format!(" (HTTP {s})")).unwrap_or_default())]
pub struct ProviderError {
    /// What went wrong, in the router's terms.
    pub kind: ErrorKind,
    /// The HTTP status when there was one.
    pub status: Option<u16>,
}

impl ProviderError {
    /// An error with no HTTP status (connect failure, timeout, bad stream).
    pub fn new(kind: ErrorKind) -> ProviderError {
        ProviderError { kind, status: None }
    }
}

/// Every way a call can fail, closed so retry rules are a `match` (MODEL_ROUTING §7).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ErrorKind {
    #[error("the provider refused the credentials")]
    Auth,
    #[error("the provider account is out of credits")]
    InsufficientCredits,
    #[error("rate limited")]
    RateLimited {
        /// From `Retry-After`, when the provider sent one.
        retry_after: Option<Duration>,
    },
    #[error("the provider is overloaded")]
    Overloaded,
    #[error("the provider failed")]
    Server,
    #[error("the provider rejected the request")]
    BadRequest,
    #[error("the prompt is longer than the model's context")]
    ContextLength,
    #[error("the provider refused the content")]
    ContentPolicy,
    #[error("timed out ({0})")]
    Timeout(TimeoutKind),
    #[error("couldn't connect to the provider")]
    Connect,
    #[error("canceled")]
    Canceled,
    /// The `&'static str` says which rule the response broke, never what it held.
    #[error("invalid response: {0}")]
    InvalidResponse(&'static str),
    #[error("response too large: {0}")]
    TooLarge(&'static str),
    #[error("the call would leave this machine, which the run doesn't allow")]
    LocalityViolation,
    #[error("budget exhausted: {0}")]
    BudgetExhausted(Limit),
}

/// Which timeout fired (MODEL_ROUTING §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimeoutKind {
    #[error("no first token")]
    FirstToken,
    #[error("total")]
    Total,
}

/// Which run ceiling was hit (MODEL_ROUTING §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Limit {
    #[error("input tokens")]
    InputTokens,
    #[error("output tokens")]
    OutputTokens,
    #[error("cost")]
    Cost,
    #[error("deadline")]
    Deadline,
}

/// Why `[models]` plus the environment don't give a usable set of bindings. Names
/// keys and variables, never their values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// A startup error, not a runtime surprise (MODEL_ROUTING §3).
    #[error("no model binding for the required role {}; add one for its class in [models.bindings], or set it in [models.roles]", crate::wire_id(.0))]
    MissingBinding(ModelRole),
    #[error("{key}: names a binding that isn't defined")]
    UnknownBinding {
        /// The config key holding the bad reference, like `models.roles.judge`.
        key: String,
    },
    #[error("{var} is not set; an {provider} binding needs it")]
    MissingCredential {
        /// The environment variable to set.
        var: &'static str,
        /// The provider that needs it.
        provider: &'static str,
    },
    #[error("{0}: must be an https:// origin with no path (plain http only for localhost)")]
    BadEndpoint(String),
}
