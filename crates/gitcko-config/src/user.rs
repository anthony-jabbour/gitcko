// SPDX-License-Identifier: AGPL-3.0-or-later

use gitcko_protocol::config::{ModelProvider, UserConfig};

/// Whether `gitcko setup` (or an interactive review) has recorded consent for this
/// remote provider (PRIVACY, F-39).
pub fn has_remote_acknowledgement(config: &UserConfig, provider: ModelProvider) -> bool {
    let want = match provider {
        ModelProvider::Openrouter => "openrouter",
        ModelProvider::Ollama => return false,
    };
    config
        .privacy
        .as_ref()
        .and_then(|privacy| privacy.remote_acknowledged.as_ref())
        .is_some_and(|names| names.iter().any(|name| name == want))
}
