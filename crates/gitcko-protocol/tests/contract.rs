// SPDX-License-Identifier: Apache-2.0

//! The wire contract: every documented field parses, survives a round trip unchanged,
//! and the forward-compatibility rules hold.

use gitcko_protocol::config::{RepoConfig, UserConfig};
use gitcko_protocol::event::{RunEvent, RunEventKind};
use gitcko_protocol::review::Review;
use serde_json::Value;

const REVIEW: &str = include_str!("data/review-full.json");

fn review_value() -> Value {
    serde_json::from_str(REVIEW).unwrap()
}

#[test]
fn full_review_round_trips_unchanged() {
    let review: Review = serde_json::from_str(REVIEW).unwrap();
    // Compare as JSON values, so field order and whitespace don't matter but every
    // field and value must come back exactly as it went in.
    assert_eq!(serde_json::to_value(&review).unwrap(), review_value());
}

#[test]
fn full_review_snapshot() {
    let review: Review = serde_json::from_str(REVIEW).unwrap();
    insta::assert_json_snapshot!(review);
}

#[test]
fn review_schema_version_is_checked() {
    let mut value = review_value();
    value["schema_version"] = "gitcko.review/2".into();
    let err = serde_json::from_value::<Review>(value).unwrap_err();
    assert!(err.to_string().contains("gitcko.review/1"), "{err}");
}

#[test]
fn review_reader_tolerates_fields_added_later() {
    // CLI_SPEC §6: additive fields don't bump the version, so an older reader must
    // accept a newer file.
    let mut value = review_value();
    value["added_in_a_later_release"] = true.into();
    value["findings"][0]["also_new"] = 1.into();
    serde_json::from_value::<Review>(value).unwrap();
}

#[test]
fn clean_review_has_empty_findings_not_null() {
    let mut value = review_value();
    value["findings"] = Value::Array(vec![]);
    let review: Review = serde_json::from_value(value).unwrap();
    assert_eq!(
        serde_json::to_value(&review).unwrap()["findings"],
        Value::Array(vec![])
    );
}

#[test]
fn events_are_omitted_unless_present() {
    let mut value = review_value();
    value.as_object_mut().unwrap().remove("events");
    let review: Review = serde_json::from_value(value).unwrap();
    assert!(
        serde_json::to_value(&review)
            .unwrap()
            .get("events")
            .is_none()
    );
}

#[test]
fn unknown_run_event_type_is_kept_as_unknown() {
    // Cloud adds private event types (credits.*) that the public protocol doesn't define.
    let json = r#"{"run_id":"rv_x","seq":3,"at":"2026-10-17T18:02:11Z","type":"credits.reserved","amount":3}"#;
    let event: RunEvent = serde_json::from_str(json).unwrap();
    assert_eq!(event.kind, RunEventKind::Unknown);
}

#[test]
fn full_repo_config_parses_every_documented_key() {
    let config: RepoConfig = toml::from_str(include_str!("data/config-full.toml")).unwrap();
    insta::assert_json_snapshot!(config);
}

#[test]
fn repo_config_rejects_unknown_keys() {
    // CONFIGURATION §11: unknown keys are an error, never ignored.
    let err = toml::from_str::<RepoConfig>("strictnes = 2").unwrap_err();
    assert!(err.to_string().contains("strictnes"), "{err}");
}

#[test]
fn repo_config_rejects_models() {
    // CONFIGURATION §2: repository files can never set models.
    assert!(toml::from_str::<RepoConfig>("[models.bindings.smart]\nprovider = \"x\"").is_err());
}

#[test]
fn full_user_config_parses_every_documented_key() {
    let config: UserConfig = toml::from_str(include_str!("data/user-config-full.toml")).unwrap();
    insta::assert_json_snapshot!(config);
}

#[test]
fn every_schema_is_draft_2020_12_with_a_stable_id() {
    let schemas = gitcko_protocol::schemas();
    let names: Vec<_> = schemas.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        ["config-1", "review-1", "run-event-1", "user-config-1"]
    );
    for (name, schema) in schemas {
        let value = schema.to_value();
        assert_eq!(
            value["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(
            value["$id"],
            format!("https://gitcko.dev/schema/{name}.json")
        );
    }
}

#[test]
fn review_schema_caps_questions_at_two() {
    // REVIEW_RULES §8: at most two questions per review, so tools that validate output
    // against the schema can enforce it too.
    let (_, schema) = gitcko_protocol::schemas()
        .into_iter()
        .find(|(name, _)| *name == "review-1")
        .unwrap();
    assert_eq!(schema.to_value()["properties"]["questions"]["maxItems"], 2);
}
