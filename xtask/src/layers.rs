// SPDX-License-Identifier: AGPL-3.0-or-later

//! Crates may only depend on crates in their own or a lower layer, so the
//! review engine never pulls in a CLI, TUI or server framework.

use crate::{Result, output};

/// Every workspace crate and its layer. A new crate must be added here, which
/// makes its place in the stack an explicit, reviewed decision.
const LAYERS: &[(&str, u8)] = &[
    // 0: foundation types and helpers
    ("gitcko-protocol", 0),
    ("gitcko-domain", 0),
    ("gitcko-config", 0),
    ("gitcko-redact", 0),
    ("gitcko-telemetry", 0),
    // Shared test helpers sit at the bottom so every crate's tests can use them.
    ("gitcko-test-support", 0),
    // 1: git and diffs
    ("gitcko-git", 1),
    ("gitcko-diff", 1),
    // 2: indexing
    ("gitcko-syntax", 2),
    ("gitcko-graph", 2),
    ("gitcko-search", 2),
    ("gitcko-embed", 2),
    ("gitcko-index", 2),
    // 3: models
    ("gitcko-model", 3),
    ("gitcko-model-openai", 3),
    ("gitcko-prompts", 3),
    ("gitcko-cassette", 3),
    // 4: review
    ("gitcko-rules", 4),
    ("gitcko-tools", 4),
    ("gitcko-validate", 4),
    ("gitcko-scoring", 4),
    ("gitcko-fix", 4),
    ("gitcko-verifier", 4),
    ("gitcko-storage", 4),
    ("gitcko-review", 4),
    // 5: surfaces
    ("gitcko-render", 5),
    ("gitcko-tui", 5),
    ("gitcko-mcp", 5),
    ("gitcko-cli", 5),
    ("gitcko-config-import", 5),
    // 6: server
    ("gitcko-storage-pg", 6),
    ("gitcko-jobs", 6),
    ("gitcko-server", 6),
    ("gitcko-scm", 6),
    ("gitcko-scm-github", 6),
    ("gitcko-scm-origin", 6),
    ("gitcko-verify-gvisor", 6),
];

/// Dev tooling that sits outside the layers.
const EXEMPT: &[&str] = &["xtask"];

fn layer(name: &str) -> Option<u8> {
    LAYERS.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

/// `packages` is each workspace crate with the workspace crates it depends on.
fn violations(packages: &[(String, Vec<String>)]) -> Vec<String> {
    let mut found = Vec::new();
    for (name, deps) in packages {
        if EXEMPT.contains(&name.as_str()) {
            continue;
        }
        let Some(own) = layer(name) else {
            found.push(format!(
                "{name}: not in the LAYERS table in xtask/src/layers.rs"
            ));
            continue;
        };
        for dep in deps {
            match layer(dep) {
                Some(theirs) if theirs > own => found.push(format!(
                    "{name} (layer {own}) depends on {dep} (layer {theirs}); only same or lower layers are allowed"
                )),
                Some(_) => {}
                None => found.push(format!("{name} depends on {dep}, which is not in the LAYERS table")),
            }
        }
    }
    found
}

/// Reads workspace crates and their path (in-workspace) dependencies from `cargo metadata`.
fn workspace_packages() -> Result<Vec<(String, Vec<String>)>> {
    let json = output("cargo", &["metadata", "--format-version", "1", "--no-deps"])?;
    let meta: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let packages = meta["packages"]
        .as_array()
        .ok_or("cargo metadata: no packages")?;
    Ok(packages
        .iter()
        .map(|p| {
            let deps = p["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|d| d["path"].is_string())
                .filter_map(|d| d["name"].as_str().map(str::to_string))
                .collect();
            (p["name"].as_str().unwrap_or_default().to_string(), deps)
        })
        .collect())
}

pub fn check() -> Result {
    let packages = workspace_packages()?;
    let found = violations(&packages);
    if found.is_empty() {
        println!("layers: {} crates ok", packages.len());
        Ok(())
    } else {
        Err(found.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, deps: &[&str]) -> (String, Vec<String>) {
        (name.into(), deps.iter().map(|d| d.to_string()).collect())
    }

    #[test]
    fn same_or_lower_layer_is_fine() {
        let pkgs = [pkg(
            "gitcko-review",
            &["gitcko-domain", "gitcko-scoring", "gitcko-index"],
        )];
        assert!(violations(&pkgs).is_empty());
    }

    #[test]
    fn upward_dependency_fails() {
        let pkgs = [pkg("gitcko-review", &["gitcko-cli"])];
        assert_eq!(violations(&pkgs).len(), 1);
    }

    #[test]
    fn unknown_crate_fails() {
        assert_eq!(violations(&[pkg("gitcko-new", &[])]).len(), 1);
        assert_eq!(violations(&[pkg("gitcko-cli", &["gitcko-new"])]).len(), 1);
    }

    #[test]
    fn xtask_is_exempt() {
        assert!(violations(&[pkg("xtask", &["anything"])]).is_empty());
    }
}
