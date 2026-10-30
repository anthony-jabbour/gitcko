#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
/// `page` starts at 0.
pub fn page(items: &[u32], page: usize, size: usize) -> &[u32] {
    let start = (page * size).min(items.len());
    let end = (start + size).min(items.len());
    &items[start..end]
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
/// `page` starts at 0.
pub fn page(items: &[u32], page: usize, size: usize) -> &[u32] {
    let start = ((page - 1) * size).min(items.len());
    let end = (start + size).min(items.len());
    &items[start..end]
}
GITCKO
commit_head "Fix page offset"
