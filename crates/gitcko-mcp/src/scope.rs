// SPDX-License-Identifier: AGPL-3.0-or-later

//! Optional path scope recorded when a review run used `paths` (MCP or CLI).

use std::fs;
use std::path::PathBuf;

use gitcko_domain::RepoPath;
use gitcko_storage::ReviewStore;

fn scope_path(store: &ReviewStore, repo_id: &str, review_id: &str) -> PathBuf {
    store
        .root()
        .join("repos")
        .join(repo_id)
        .join("reviews")
        .join(format!("{review_id}.scope"))
}

pub fn write_scope(store: &ReviewStore, repo_id: &str, review_id: &str, paths: &[RepoPath]) {
    if paths.is_empty() {
        return;
    }
    let strings: Vec<String> = paths.iter().map(|path| path.as_str().to_string()).collect();
    let path = scope_path(store, repo_id, review_id);
    if let Ok(bytes) = serde_json::to_vec(&strings) {
        let _ = fs::write(path, bytes);
    }
}

pub fn read_scope(store: &ReviewStore, repo_id: &str, review_id: &str) -> Option<Vec<String>> {
    let path = scope_path(store, repo_id, review_id);
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn covers_path(scopes: Option<&[String]>, finding_path: &str) -> bool {
    match scopes {
        None | Some([]) => true,
        Some(list) => list.iter().any(|path| path == finding_path),
    }
}
