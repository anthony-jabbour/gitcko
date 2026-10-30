#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
/// `x` limited to the range 0 to 1.
pub fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}
GITCKO
commit_head "Document clamp01"
