// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeSet;

use gitcko_protocol::review::{Finding, Lifecycle};

/// Sets each current finding to `new` or `still_present` from the previous review of
/// the same branch, and returns the previous ids that are gone (`resolved`).
/// `possibly_fixed` is not produced here (needs anchor tracking, P6).
pub fn reconcile(previous: &[Finding], current: &mut [Finding]) -> Vec<String> {
    let prev_ids: BTreeSet<&str> = previous.iter().map(|f| f.id.as_str()).collect();
    let curr_ids: BTreeSet<String> = current.iter().map(|f| f.id.clone()).collect();
    for finding in current.iter_mut() {
        finding.lifecycle = if prev_ids.contains(finding.id.as_str()) {
            Lifecycle::StillPresent
        } else {
            Lifecycle::New
        };
    }
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::new();
    for finding in previous {
        if !curr_ids.contains(&finding.id) && seen.insert(finding.id.as_str()) {
            resolved.push(finding.id.clone());
        }
    }
    resolved
}
