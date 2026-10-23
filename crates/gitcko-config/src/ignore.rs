// SPDX-License-Identifier: AGPL-3.0-or-later

//! Matching paths against `ignore` lists, with gitignore semantics: the key is documented
//! as gitignore syntax, as Greptile's `ignorePatterns` is [DOCS]. Plain globs can't do
//! negation (`!keep.rs`), anchoring (`/build`) or folder-only patterns (`tmp/`), so this
//! uses the `ignore` crate's gitignore matcher (from ripgrep), on paths from the
//! repository's tree. Its directory walker is never used: nothing here reads the disk.

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use gitcko_domain::RepoPath;

use crate::{Ceilings, ConfigError, Layer, RepoFiles, RunFlags, resolve_repo};

/// The effective `ignore` list of every folder that has a `.gitcko/`, ready to match.
pub struct IgnoreRules {
    /// `(folder prefix, base prefix, matcher)`, deepest folder first. The prefixes are
    /// `""` for the root or `"pkg/"`; the base is the folder whose config set the list,
    /// which patterns are relative to (a folder without its own list inherits its
    /// parent's, still relative to the parent).
    folders: Vec<(String, String, Gitignore)>,
}

/// Builds the matchers from the repository's config files, resolving each folder the
/// way a review there would (closest config wins, arrays replace).
pub fn ignore_rules(files: &RepoFiles) -> Result<IgnoreRules, ConfigError> {
    let mut prefixes: Vec<String> = files
        .folders
        .iter()
        .filter_map(|f| f.folder.as_ref().map(|p| format!("{}/", p.as_str())))
        .collect();
    prefixes.push(String::new());
    prefixes.sort_by_key(|p| std::cmp::Reverse(p.matches('/').count()));
    prefixes.dedup();

    let mut folders = Vec::new();
    for prefix in prefixes {
        // The folders are already valid `RepoPath`s, so adding a plain file name can't fail.
        let probe = RepoPath::parse(&format!("{prefix}.gitcko/config.toml"))
            .expect("a valid folder plus a plain file name is a valid path");
        let resolved = resolve_repo(files, &probe, &RunFlags::default(), &Ceilings::local())?;
        let Some(entry) = resolved.get("ignore") else {
            folders.push((prefix, String::new(), Gitignore::empty()));
            continue;
        };
        let source = entry.sources.last().expect("an entry has a source");
        let base = match (&source.layer, &source.file) {
            (Layer::Repo, Some(file)) => file
                .strip_suffix(".gitcko/config.toml")
                .unwrap_or_default()
                .to_string(),
            _ => String::new(),
        };
        // Names the file and key, never the pattern, which comes from the repository.
        let invalid = || ConfigError {
            file: source.file.clone().unwrap_or_else(|| source.to_string()),
            line: source.line,
            key: Some("ignore".into()),
            message: "not a valid gitignore pattern".into(),
        };
        // An empty root: paths are matched relative to `base` in string form, so the
        // result doesn't depend on the OS's path rules.
        let mut builder = GitignoreBuilder::new("");
        for pattern in entry.value.as_array().into_iter().flatten() {
            let pattern = pattern.as_str().unwrap_or_default();
            builder.add_line(None, pattern).map_err(|_| invalid())?;
        }
        let matcher = builder.build().map_err(|_| invalid())?;
        folders.push((prefix, base, matcher));
    }
    Ok(IgnoreRules { folders })
}

impl IgnoreRules {
    /// Whether `path` matches the `ignore` list of the deepest folder containing it.
    pub fn is_ignored(&self, path: &RepoPath) -> bool {
        let path = path.as_str();
        let Some((_, base, matcher)) = self
            .folders
            .iter()
            .find(|(prefix, _, _)| path.starts_with(prefix.as_str()))
        else {
            return false;
        };
        let relative = path.strip_prefix(base.as_str()).unwrap_or(path);
        matcher
            .matched_path_or_any_parents(relative, false)
            .is_ignore()
    }
}
