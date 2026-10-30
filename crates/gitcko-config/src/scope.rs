// SPDX-License-Identifier: AGPL-3.0-or-later

//! `scope` globs on `[[rules]]` and `[[context.files]]`: which reviewed files a rule or
//! context file applies to. Globs are relative to the folder whose `.gitcko/` defines
//! them (CONFIGURATION §5), so `src/db/**` in `pkg/.gitcko/` means `pkg/src/db/**`.
//!
//! Plain globs via `globset` (from ripgrep), with `*` stopping at `/` and `**` crossing
//! folders, as in gitignore. Unlike `ignore` there is no negation: a scope only narrows.

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use serde_json::Value;

use gitcko_domain::RepoPath;
use gitcko_protocol::config::{ContextFile, Rule};

use crate::{Layer, Resolved, Source};

/// One glob, compiled the same way at load (to refuse a bad one) and at match time.
pub(crate) fn glob(pattern: &str) -> Result<Glob, globset::Error> {
    GlobBuilder::new(pattern).literal_separator(true).build()
}

/// The folder prefix (`""` or `"pkg/"`) of the config file an item came from.
fn base(source: &Source) -> &str {
    match (&source.layer, &source.file) {
        (Layer::Repo, Some(file)) => file.strip_suffix(".gitcko/config.toml").unwrap_or(""),
        _ => "",
    }
}

/// True when `scope` is absent or one of its globs, joined to `base`, matches `path`.
fn in_scope(base: &str, scope: Option<&[String]>, path: &RepoPath) -> bool {
    let Some(scope) = scope else { return true };
    let mut set = GlobSetBuilder::new();
    for pattern in scope {
        // Checked when the file was loaded, so this can't fail.
        set.add(glob(&format!("{base}{pattern}")).expect("validated at load"));
    }
    let set: GlobSet = set.build().expect("validated at load");
    set.is_match(path.as_str())
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
