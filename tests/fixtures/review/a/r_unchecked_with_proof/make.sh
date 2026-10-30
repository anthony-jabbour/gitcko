#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn pick(xs: &[u8], seed: usize) -> u8 {
    assert!(!xs.is_empty());
    xs[seed % xs.len()]
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn pick(xs: &[u8], seed: usize) -> u8 {
    assert!(!xs.is_empty());
    let i = seed % xs.len();
    // SAFETY: `i` is below `xs.len()`, and the assert rules out a zero length.
    unsafe { *xs.get_unchecked(i) }
}
GITCKO
commit_head "Skip the second bounds check in pick"
