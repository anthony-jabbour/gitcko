#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn first_or_zero(xs: &[u32]) -> u32 {
    match xs.first() {
        Some(x) => *x,
        None => 0,
    }
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn first_or_zero(xs: &[u32]) -> u32 {
    if xs.is_empty() {
        return 0;
    }
    *xs.first().unwrap()
}
GITCKO
commit_head "Return early on empty input"
