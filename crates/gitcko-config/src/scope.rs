// SPDX-License-Identifier: AGPL-3.0-or-later

//! `scope` globs on `[[rules]]` and `[[context.files]]`: which reviewed files a rule or
//! context file applies to. Globs are relative to the folder whose `.gitcko/` defines
//! them (CONFIGURATION §5), so `src/db/**` in `pkg/.gitcko/` means `pkg/src/db/**`.
//!
//! Plain globs via `globset` (from ripgrep), with `*` stopping at `/` and `**` crossing
//! folders, as in gitignore. Unlike `ignore` there is no negation: a scope only narrows.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde_json::Value;

use gitcko_domain::RepoPath;
use gitcko_protocol::config::{ContextFile, Rule};

use crate::{Layer, Resolved, Source};

/// One `scope` list, compiled exactly as written: at load to refuse a bad list, and
/// again at match time, so what was validated is what runs.
pub(crate) fn scope_set(patterns: &[String]) -> Result<GlobSet, globset::Error> {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        set.add(GlobBuilder::new(pattern).literal_separator(true).build()?);
    }
    set.build()
}

/// The folder prefix (`""` or `"pkg/"`) of the config file an item came from.
fn base(source: &Source) -> &str {
    match (&source.layer, &source.file) {
        (Layer::Repo, Some(file)) => file.strip_suffix(".gitcko/config.toml").unwrap_or(""),
        _ => "",
    }
}

/// True when `scope` is absent, or `path` is under `base` and one of the globs matches
/// the rest of it. The folder is stripped from the path rather than joined to the
/// patterns, so glob characters in a folder name (`lib[1]/`) stay literal.
fn in_scope(base: &str, scope: Option<&[String]>, path: &RepoPath) -> bool {
    let Some(scope) = scope else { return true };
    let Some(rest) = path.as_str().strip_prefix(base) else {
        return false;
    };
    // The same list compiled at load, so this can't fail.
    scope_set(scope).expect("validated at load").is_match(rest)
}

impl Resolved {
    /// The combined `[[rules]]` that apply to `path`, root first, in file order.
    pub fn rules_for(&self, path: &RepoPath) -> Vec<Rule> {
        self.scoped("rules", path, |_: &mut Rule, _| {})
    }

    /// The combined `[[context.files]]` that apply to `path`, with each `path` made
    /// repository-relative so the caller can read it without knowing its folder.
    pub fn context_files_for(&self, path: &RepoPath) -> Vec<ContextFile> {
        self.scoped("context.files", path, |file: &mut ContextFile, base| {
            file.path = format!("{base}{}", file.path);
        })
    }

    fn scoped<T: serde::de::DeserializeOwned + Scoped>(
        &self,
        key: &str,
        path: &RepoPath,
        fix: impl Fn(&mut T, &str),
    ) -> Vec<T> {
        let Some(entry) = self.get(key) else {
            return Vec::new();
        };
        let items = entry
            .value
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        // Combined lists keep one source per item (`Resolved::apply`).
        items
            .iter()
            .zip(&entry.sources)
            .filter_map(|(item, source): (&Value, _)| {
                let mut item: T = serde_json::from_value(item.clone()).expect("validated");
                let base = base(source);
                in_scope(base, item.scope(), path).then(|| {
                    fix(&mut item, base);
                    item
                })
            })
            .collect()
    }
}

trait Scoped {
    fn scope(&self) -> Option<&[String]>;
}

impl Scoped for Rule {
    fn scope(&self) -> Option<&[String]> {
        self.scope.as_deref()
    }
}

impl Scoped for ContextFile {
    fn scope(&self) -> Option<&[String]> {
        self.scope.as_deref()
    }
}
