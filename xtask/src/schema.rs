// SPDX-License-Identifier: AGPL-3.0-or-later

//! JSON Schemas in `integrations/schemas/` are generated from `gitcko-protocol`, so the
//! published contract can't drift from the Rust types (like Greptile's `generate:schema`
//! step for its CLI settings schema).

use std::collections::BTreeMap;
use std::fs;

use crate::Result;

const DIR: &str = "integrations/schemas";

/// Writes every schema and removes any `.json` file in the folder that is no longer
/// generated, so a renamed schema doesn't leave its old copy behind.
pub fn generate() -> Result {
    fs::create_dir_all(DIR).map_err(|e| format!("{DIR}: {e}"))?;
    let generated = generated();
    for (name, _) in committed()? {
        if !generated.iter().any(|(n, _)| *n == name) {
            fs::remove_file(format!("{DIR}/{name}")).map_err(|e| format!("{name}: {e}"))?;
        }
    }
    for (name, text) in &generated {
        fs::write(format!("{DIR}/{name}"), text).map_err(|e| format!("{name}: {e}"))?;
    }
    println!("schema-gen: wrote {} schemas to {DIR}", generated.len());
    Ok(())
}

/// Fails if the committed schemas differ in any way from what the types generate.
pub fn check() -> Result {
    let problems = differences(&generated(), &committed()?);
    if problems.is_empty() {
        println!("schema-check: {DIR} matches gitcko-protocol");
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// `(file name, file text)` for each schema: pretty JSON with a final newline, so the
/// files diff cleanly in review.
fn generated() -> Vec<(String, String)> {
    gitcko_protocol::schemas()
        .into_iter()
        .map(|(stem, schema)| {
            let text = serde_json::to_string_pretty(&schema).expect("a schema is valid JSON");
            (format!("{stem}.json"), text + "\n")
        })
        .collect()
}

fn committed() -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    let entries = match fs::read_dir(DIR) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(e) => return Err(format!("{DIR}: {e}")),
    };
    for entry in entries {
        let path = entry.map_err(|e| format!("{DIR}: {e}"))?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = fs::read_to_string(&path).map_err(|e| format!("{name}: {e}"))?;
            files.insert(name, text);
        }
    }
    Ok(files)
}

/// One message per file that is stale, missing or no longer generated.
fn differences(
    generated: &[(String, String)],
    committed: &BTreeMap<String, String>,
) -> Vec<String> {
    const FIX: &str = "run `cargo xtask schema-gen` and commit the result";
    let mut problems = Vec::new();
    for (name, text) in generated {
        match committed.get(name) {
            None => problems.push(format!("{DIR}/{name} is missing; {FIX}")),
            Some(old) if old != text => {
                problems.push(format!("{DIR}/{name} is out of date with the types; {FIX}"))
            }
            Some(_) => {}
        }
    }
    for name in committed.keys() {
        if !generated.iter().any(|(n, _)| n == name) {
            problems.push(format!("{DIR}/{name} is no longer generated; {FIX}"));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn committed_copy() -> BTreeMap<String, String> {
        generated().into_iter().collect()
    }

    #[test]
    fn up_to_date_schemas_pass() {
        assert_eq!(
            differences(&generated(), &committed_copy()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn type_change_without_regenerating_fails() {
        // Stand-in for a field added to a protocol type: the generated text changes
        // while the committed copy stays as it was.
        let mut changed = generated();
        let (_, text) = changed
            .iter_mut()
            .find(|(n, _)| n == "review-1.json")
            .unwrap();
        *text = text.replacen(
            "\"properties\": {",
            "\"properties\": {\n    \"new_field\": {},",
            1,
        );
        let problems = differences(&changed, &committed_copy());
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("review-1.json") && problems[0].contains("schema-gen"));
    }

    #[test]
    fn hand_edited_schema_fails() {
        let mut committed = committed_copy();
        committed.get_mut("config-1.json").unwrap().push(' ');
        assert_eq!(differences(&generated(), &committed).len(), 1);
    }

    #[test]
    fn missing_and_leftover_files_fail() {
        let mut committed = committed_copy();
        committed.remove("run-event-1.json");
        committed.insert("old-0.json".into(), "{}\n".into());
        let problems = differences(&generated(), &committed);
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn generated_files_are_pretty_json_ending_in_newline() {
        for (name, text) in generated() {
            assert!(name.ends_with(".json"));
            assert!(text.ends_with("}\n") && text.contains("\n  \""), "{name}");
        }
    }
}
