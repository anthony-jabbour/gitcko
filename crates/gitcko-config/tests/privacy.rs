// SPDX-License-Identifier: AGPL-3.0-or-later

//! `privacy.remote_acknowledged` is user config (F-39). A canary value is never echoed.

use std::collections::BTreeMap;

use gitcko_config::{RunFlags, has_remote_acknowledgement, resolve_user};
use gitcko_protocol::config::ModelProvider;
use gitcko_redact::canaries;

fn user(text: &str) -> Result<gitcko_config::Resolved, gitcko_config::ConfigError> {
    resolve_user(Some(text), &BTreeMap::new(), &RunFlags::default())
}

#[test]
fn openrouter_acknowledgement_is_read_from_user_config() {
    let resolved = user("[privacy]\nremote_acknowledged = [\"openrouter\"]\n").unwrap();
    let config = resolved.user_config();
    assert!(has_remote_acknowledgement(
        &config,
        ModelProvider::Openrouter
    ));
    assert!(!has_remote_acknowledgement(&config, ModelProvider::Ollama));
}

#[test]
fn missing_privacy_table_means_nothing_is_acknowledged() {
    let config = user("color = true\n").unwrap().user_config();
    assert!(!has_remote_acknowledgement(
        &config,
        ModelProvider::Openrouter
    ));
}

#[test]
fn an_unknown_provider_is_rejected_without_echoing_its_name() {
    let canary = canaries()[1].clone();
    let err = user(&format!(
        "[privacy]\nremote_acknowledged = [\"{canary}\"]\n"
    ))
    .expect_err("unknown provider");
    let shown = err.to_string();
    assert!(shown.contains("privacy.remote_acknowledged"), "{shown}");
    assert!(
        !shown.contains(&canary),
        "acknowledgement value leaked: {shown}"
    );
}
