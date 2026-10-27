// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which bindings a call may try, in which order (MODEL_ROUTING §8, §9, §11), decided
//! before anything is sent.

use gitcko_protocol::config::{ModelClass, ModelRole};

use super::{Run, inversion};
use crate::{DestinationClass, InversionDecision, ResolvedBinding, ResolvedModels, class_of};

pub(crate) struct Plan<'a> {
    /// Eligible bindings, best first.
    pub candidates: Vec<&'a ResolvedBinding>,
    /// How many may be tried: one for `deep`, which never falls back.
    pub tries: usize,
    /// The configured primary when it isn't a candidate because the call would go
    /// further than the run or the primary allows.
    pub skipped_primary: Option<&'a str>,
    pub inversion: Option<InversionDecision>,
}

/// `None` when the role has no binding. ponytail: the capability floor isn't checked
/// here yet; adapters report model capabilities in Batch B, and the check joins then.
pub(crate) fn plan<'a>(models: &'a ResolvedModels, run: &Run, role: ModelRole) -> Option<Plan<'a>> {
    let primary = models.binding_for(role)?;
    let class = class_of(role);
    let mut pool = vec![primary];
    for name in models.fallbacks.get(&class).into_iter().flatten() {
        if let Some(binding) = models.bindings.get(name)
            && !pool.iter().any(|b| b.name == binding.name)
        {
            pool.push(binding);
        }
    }
    let configured = pool.clone();
    // Same or stricter destination only: a local primary never falls back to a remote
    // one, and a local-only run never leaves the device.
    let ceiling = if run.local_only {
        DestinationClass::OnDevice
    } else {
        primary.destination.class
    };
    pool.retain(|b| b.destination.class <= ceiling);
    let skipped_primary =
        (pool.first().map(|b| &b.name) != Some(&primary.name)).then_some(primary.name.as_str());
    let inversion = inversion::order(&mut pool, &configured, role, models.inversion, run.author);
    if role == ModelRole::Judge
        && let Some(reviewer) = run.reviewer
    {
        // Independence: a family other than the reviewer's first. A stable sort, so
        // the configured order holds within each group.
        pool.sort_by_key(|b| b.family == reviewer);
    }
    let tries = if class == ModelClass::Deep {
        1
    } else {
        pool.len()
    };
    Some(Plan {
        candidates: pool,
        tries,
        skipped_primary,
        inversion,
    })
}
