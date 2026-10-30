#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn squares(xs: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(xs.len());
    for x in xs {
        out.push(x * x);
    }
    out
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn squares(xs: &[u32]) -> Vec<u32> {
    xs.iter().map(|x| x * x).collect()
}
GITCKO
commit_head "Use an iterator in squares"
