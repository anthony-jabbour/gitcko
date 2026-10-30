#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::path::{Component, Path, PathBuf};

pub fn resolve(root: &Path, name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    let safe = path.components().all(|c| matches!(c, Component::Normal(_)));
    safe.then(|| root.join(path))
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::path::{Path, PathBuf};

pub fn resolve(root: &Path, name: &str) -> Option<PathBuf> {
    Some(root.join(Path::new(name)))
}
GITCKO
commit_head "Simplify resolve"
