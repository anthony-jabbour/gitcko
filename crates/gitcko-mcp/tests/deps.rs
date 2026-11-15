// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko-mcp` does not depend on reqwest directly. A path that reaches reqwest
//! has to go through a model adapter, which only `gitcko_review` constructs.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

#[test]
fn the_manifest_does_not_name_reqwest() {
    let text = std::fs::read_to_string(manifest()).unwrap();
    let deps = text
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("[dev-dependencies]")
        .next()
        .unwrap();
    assert!(
        !deps.lines().any(|line| {
            let key = line.split('=').next().unwrap_or("").trim();
            key == "reqwest"
        }),
        "reqwest must not be a direct dependency:\n{deps}"
    );
}

#[test]
fn reqwest_is_reached_only_through_a_model_adapter() {
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(manifest())
        .output()
        .expect("cargo metadata");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut names: BTreeMap<String, String> = BTreeMap::new();
    for package in meta["packages"].as_array().unwrap() {
        names.insert(
            package["id"].as_str().unwrap().to_string(),
            package["name"].as_str().unwrap().to_string(),
        );
    }
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for node in meta["resolve"]["nodes"].as_array().unwrap() {
        let name = names.get(node["id"].as_str().unwrap()).unwrap().clone();
        let deps = node["deps"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|dep| names.get(dep["pkg"].as_str().unwrap()).cloned())
            .collect();
        graph.insert(name, deps);
    }
    let mut queue = VecDeque::from([("gitcko-mcp".to_string(), Vec::<String>::new())]);
    let mut seen = BTreeSet::from(["gitcko-mcp".to_string()]);
    let mut found_reqwest = false;
    while let Some((name, path)) = queue.pop_front() {
        if name == "reqwest" {
            found_reqwest = true;
            let through_adapter = path
                .iter()
                .any(|step| step == "gitcko-model-ollama" || step == "gitcko-model-openai");
            assert!(
                through_adapter,
                "reqwest reached gitcko-mcp without a model adapter: {path:?}"
            );
            continue;
        }
        for dep in graph.get(&name).into_iter().flatten() {
            if seen.insert(dep.clone()) {
                let mut next = path.clone();
                next.push(dep.clone());
                queue.push_back((dep.clone(), next));
            }
        }
    }
    let _ = found_reqwest;
}

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
}
