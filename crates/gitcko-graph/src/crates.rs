// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where Rust crates and modules are: `Cargo.toml` files say which crate targets exist
//! and where their root files are; `mod x;` declarations, followed from each root, say
//! which file is which module. Manifests are read as data, exactly as cargo would read
//! them, but nothing is run (IX-031).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use gitcko_domain::RepoPath;

use crate::{FileNode, GraphInput};

/// One compilation target: a library, binary, test, bench, example or build script, with
/// the file its module tree starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateUnit {
    /// The name code uses for it (`my_core` for package `my-core`).
    pub name: String,
    /// Where its module tree starts.
    pub root: RepoPath,
    /// `lib`, `bin`, `test`, `bench`, `example` or `build`.
    pub target: String,
}

/// The crate targets, and every input file's crate and module.
pub(crate) struct Layout {
    pub crates: Vec<CrateUnit>,
    pub files: Vec<FileNode>,
}

/// Finds crate targets in the manifests and walks each one's module tree.
pub(crate) fn layout(input: &GraphInput<'_>) -> Layout {
    let present: BTreeSet<&str> = input.files.iter().map(|f| f.path.as_str()).collect();
    let mut crates = Vec::new();
    // Packages, by folder prefix, for files no crate root reaches.
    let mut packages: Vec<(String, String)> = Vec::new();
    let mut manifests: Vec<_> = input.manifests.iter().collect();
    manifests.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, text) in manifests {
        let dir = path.as_str().strip_suffix("Cargo.toml").unwrap_or_default();
        let Ok(table) = text.parse::<toml::Table>() else {
            continue;
        };
        let Some(package) = table
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
        else {
            continue;
        };
        let lib_name = table
            .get("lib")
            .and_then(|l| l.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or(package)
            .replace('-', "_");
        packages.push((dir.to_string(), lib_name.clone()));
        let mut add = |name: String, file: &str, target: &str| {
            let Ok(root) = RepoPath::parse(&normalize(&format!("{dir}{file}"))) else {
                return;
            };
            if present.contains(root.as_str()) && !crates.iter().any(|c: &CrateUnit| c.root == root)
            {
                crates.push(CrateUnit {
                    name,
                    root,
                    target: target.to_string(),
                });
            }
        };
        let lib_path = table
            .get("lib")
            .and_then(|l| l.get("path"))
            .and_then(|p| p.as_str())
            .unwrap_or("src/lib.rs");
        add(lib_name, lib_path, "lib");
        for bin in table
            .get("bin")
            .and_then(|b| b.as_array())
            .into_iter()
            .flatten()
        {
            let name = bin.get("name").and_then(|n| n.as_str()).unwrap_or(package);
            let default = format!("src/bin/{name}.rs");
            let file = bin.get("path").and_then(|p| p.as_str()).unwrap_or(&default);
            add(name.replace('-', "_"), file, "bin");
        }
        for (key, folder) in [
            ("test", "tests"),
            ("bench", "benches"),
            ("example", "examples"),
        ] {
            for entry in table
                .get(key)
                .and_then(|t| t.as_array())
                .into_iter()
                .flatten()
            {
                let Some(name) = entry.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let default = format!("{folder}/{name}.rs");
                let file = entry
                    .get("path")
                    .and_then(|p| p.as_str())
                    .unwrap_or(&default);
                add(name.replace('-', "_"), file, key);
            }
        }
        // Cargo's switches: `autobins = false` etc. turn off automatic targets (explicit
        // ones stay), and `build` names the build script or turns it off.
        let auto = |key: &str| {
            table
                .get("package")
                .and_then(|p| p.get(key))
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
        };
        if auto("autobins") {
            add(package.replace('-', "_"), "src/main.rs", "bin");
        }
        match table.get("package").and_then(|p| p.get("build")) {
            Some(toml::Value::String(path)) => add("build_script_build".into(), path, "build"),
            Some(toml::Value::Boolean(false)) => {}
            _ => add("build_script_build".into(), "build.rs", "build"),
        }
        // Cargo's automatic targets: one crate per file in these folders.
        for (folder, target, switch) in [
            ("src/bin/", "bin", "autobins"),
            ("tests/", "test", "autotests"),
            ("benches/", "bench", "autobenches"),
            ("examples/", "example", "autoexamples"),
        ] {
            if !auto(switch) {
                continue;
            }
            let prefix = format!("{dir}{folder}");
            let files: Vec<&str> = present
                .iter()
                .filter(|p| {
                    p.starts_with(&prefix) && p.ends_with(".rs") && !p[prefix.len()..].contains('/')
                })
                .copied()
                .collect();
            for file in files {
                let stem = file[prefix.len()..]
                    .trim_end_matches(".rs")
                    .replace('-', "_");
                add(stem, &file[dir.len()..], target);
            }
            // A folder target: `src/bin/tool/main.rs`, `tests/suite/main.rs`.
            let folders: Vec<&str> = present
                .iter()
                .filter_map(|p| p.strip_prefix(&prefix)?.strip_suffix("/main.rs"))
                .filter(|name| !name.contains('/'))
                .collect();
            for name in folders {
                add(
                    name.replace('-', "_"),
                    &format!("{folder}{name}/main.rs"),
                    target,
                );
            }
        }
    }

    let extraction: BTreeMap<&str, &crate::FileInput<'_>> =
        input.files.iter().map(|f| (f.path.as_str(), f)).collect();
    // A file can be in several targets (`mod shared;` in both `lib.rs` and `main.rs`), so
    // membership is per target; the first target to reach a file is its primary one.
    let mut assigned: BTreeMap<String, Vec<(usize, Vec<String>)>> = BTreeMap::new();
    for (unit, krate) in crates.iter().enumerate() {
        let mut queue = VecDeque::from([(krate.root.as_str().to_string(), Vec::<String>::new())]);
        while let Some((file, module)) = queue.pop_front() {
            let memberships = assigned.entry(file.clone()).or_default();
            if memberships.iter().any(|(u, _)| *u == unit) {
                continue;
            }
            memberships.push((unit, module.clone()));
            let Some(input) = extraction.get(file.as_str()) else {
                continue;
            };
            let x = input.extraction;
            for decl in &x.modules {
                let scope = x.scope_path(decl.scope);
                let mut child_module = module.clone();
                child_module.extend(scope.iter().cloned());
                child_module.push(decl.name.clone());
                let candidates = match &decl.path_attr {
                    // Relative to the file's folder at top level; inside inline modules, to
                    // their folders, as for a plain `mod` (the Rust reference, "The path
                    // attribute").
                    Some(path) if scope.is_empty() => {
                        vec![normalize(&format!("{}{path}", parent_dir(&file)))]
                    }
                    Some(path) => {
                        let mut base = child_dir(&file, &crates[unit].root);
                        for part in &scope {
                            base.push_str(part);
                            base.push('/');
                        }
                        vec![normalize(&format!("{base}{path}"))]
                    }
                    None => {
                        let mut base = child_dir(&file, &crates[unit].root);
                        for part in &scope {
                            base.push_str(part);
                            base.push('/');
                        }
                        vec![
                            format!("{base}{}.rs", decl.name),
                            format!("{base}{}/mod.rs", decl.name),
                        ]
                    }
                };
                if let Some(found) = candidates
                    .into_iter()
                    .find(|c| present.contains(c.as_str()))
                {
                    queue.push_back((found, child_module));
                }
            }
        }
    }

    let mut paths: Vec<&RepoPath> = input.files.iter().map(|f| &f.path).collect();
    paths.sort();
    let files = paths
        .into_iter()
        .map(|path| match assigned.get(path.as_str()) {
            Some(memberships) => FileNode {
                path: path.clone(),
                crate_unit: Some(memberships[0].0),
                module: memberships[0].1.clone(),
                module_inferred: false,
                memberships: memberships.clone(),
            },
            None => {
                // No root reaches it: guess from the path, and say it's a guess.
                let package = packages
                    .iter()
                    .filter(|(dir, _)| path.as_str().starts_with(dir.as_str()))
                    .max_by_key(|(dir, _)| dir.len());
                let unit = package.and_then(|(_, name)| {
                    crates
                        .iter()
                        .position(|c| &c.name == name && c.target == "lib")
                });
                let rest = package.map_or(path.as_str(), |(dir, _)| &path.as_str()[dir.len()..]);
                let rest = rest
                    .strip_prefix("src/")
                    .unwrap_or(rest)
                    .trim_end_matches(".rs");
                let module: Vec<String> = rest
                    .split('/')
                    .filter(|p| !matches!(*p, "mod" | "lib" | "main"))
                    .map(String::from)
                    .collect();
                FileNode {
                    path: path.clone(),
                    crate_unit: unit,
                    memberships: unit.map(|u| vec![(u, module.clone())]).unwrap_or_default(),
                    module,
                    module_inferred: true,
                }
            }
        })
        .collect();
    Layout { crates, files }
}

/// The folder of `file`, with a trailing `/` (or empty at the top).
fn parent_dir(file: &str) -> String {
    match file.rfind('/') {
        Some(i) => file[..=i].to_string(),
        None => String::new(),
    }
}

/// Where `mod x;` in `file` looks for `x.rs`: beside a crate root or a `mod.rs`, and in a
/// folder named after any other file (`a/b.rs` → `a/b/`), as rustc does.
fn child_dir(file: &str, root: &RepoPath) -> String {
    let dir = parent_dir(file);
    let name = &file[dir.len()..];
    if file == root.as_str() || name == "mod.rs" {
        dir
    } else {
        format!("{dir}{}/", name.trim_end_matches(".rs"))
    }
}

/// Resolves `.` and `..` parts, so `#[path = "../x.rs"]` and `path = "./src/lib.rs"`
/// name the file they mean. A path climbing above the repository comes back with a
/// leading `..` and fails `RepoPath::parse`.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return "../".to_string();
                }
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}
