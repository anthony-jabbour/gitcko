#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
/// Sums of each adjacent pair.
pub fn pair_sums(xs: &[i64]) -> Vec<i64> {
    let mut out = Vec::new();
    for i in 1..xs.len() {
        out.push(xs[i - 1] + xs[i]);
    }
    out
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
/// Sums of each adjacent pair.
pub fn pair_sums(xs: &[i64]) -> Vec<i64> {
    let mut out = Vec::new();
    for i in 0..xs.len() {
        out.push(xs[i] + xs[i + 1]);
    }
    out
}
GITCKO
commit_head "Index pairs from the left"
