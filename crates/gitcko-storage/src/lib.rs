// SPDX-License-Identifier: AGPL-3.0-or-later

//! Filesystem review history (LOCAL_DEPLOYMENT §5, CODEBASE §3.2).
//!
//! One JSON file per review under `<root>/repos/<repo-id>/reviews/`, directories
//! `0700` and files `0600`. A sibling `.running` marker says a run is live; if its
//! process is gone the marker is stale, never running forever. The engine does not
//! depend on this crate: surfaces write the `gitcko.review/1` result they received.

mod error;
mod lifecycle;
mod store;

pub use error::StoreError;
pub use lifecycle::reconcile;
pub use store::{ReviewStore, ReviewSummary, RunState, RunningMarker};
