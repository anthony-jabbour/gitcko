// SPDX-License-Identifier: Apache-2.0

//! Gitcko's serialized public types. Anything that leaves the process as JSON or is read
//! from a config file is defined here, and the JSON Schemas in `integrations/schemas/`
//! are generated from these types (`cargo xtask schema-gen`), never written by hand.
//!
//! This crate is Apache-2.0 so other tools can build on Gitcko's output freely, which
//! means it can't depend on any of the AGPL `gitcko-*` crates.

pub mod config;
pub mod event;
pub mod review;

use schemars::{JsonSchema, Schema, generate::SchemaSettings};

/// Every published schema as `(file stem, schema)`, sorted by name. The stem is also
/// the last part of the schema's `$id`, e.g. `https://gitcko.dev/schema/review-1.json`.
pub fn schemas() -> Vec<(&'static str, Schema)> {
    vec![
        ("config-1", schema_for::<config::RepoConfig>("config-1")),
        ("review-1", schema_for::<review::Review>("review-1")),
        ("run-event-1", schema_for::<event::RunEvent>("run-event-1")),
        (
            "user-config-1",
            schema_for::<config::UserConfig>("user-config-1"),
        ),
    ]
}

fn schema_for<T: JsonSchema>(name: &str) -> Schema {
    let mut schema = SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<T>();
    schema.insert(
        "$id".into(),
        format!("https://gitcko.dev/schema/{name}.json").into(),
    );
    schema
}
