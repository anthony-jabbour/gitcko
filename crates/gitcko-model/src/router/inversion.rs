// SPDX-License-Identifier: AGPL-3.0-or-later

//! Model inversion (MODEL_ROUTING §9): review a PR with a family other than the one
//! that wrote it. It only reorders bindings the run already allows.

use gitcko_protocol::config::ModelRole;

use crate::{AuthorFamily, InversionDecision, ResolvedBinding};

/// Puts other families' bindings first for `review` and `review-deep`, and says what
/// was decided. `configured` is every binding the role could use, before locality
/// filtering: one family there means inversion has nothing to choose from.
pub(crate) fn order(
    pool: &mut [&ResolvedBinding],
    configured: &[&ResolvedBinding],
    role: ModelRole,
    enabled: bool,
    author: AuthorFamily,
) -> Option<InversionDecision> {
    if !matches!(role, ModelRole::Review | ModelRole::ReviewDeep) {
        return None;
    }
    if !enabled {
        return Some(InversionDecision::Disabled);
    }
    let first = configured.first()?.family;
    if configured.iter().all(|b| b.family == first) {
        return Some(InversionDecision::SingleFamily);
    }
    let AuthorFamily::Known { family, .. } = author else {
        return Some(InversionDecision::AuthorUnknown);
    };
    if !pool.iter().any(|b| b.family != family) {
        return Some(InversionDecision::NoOtherFamilyAvailable);
    }
    // Stable: the configured order holds among the other families' bindings.
    pool.sort_by_key(|b| b.family == family);
    Some(InversionDecision::Applied)
}
