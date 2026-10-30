#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn sum(xs: &[u32]) -> u32 {
    xs.iter().sum()
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn sum(xs: &[u32]) -> u32 {
    let mut total = 0;
    for i in 0..xs.len() {
        total += xs[i];
    }
    total
}
GITCKO
commit_head "Write sum as an indexed loop"
