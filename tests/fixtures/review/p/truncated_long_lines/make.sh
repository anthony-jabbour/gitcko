#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}
GITCKO
commit_base
awk 'BEGIN { printf "pub const TEXT: &str = \""; for (i = 0; i < 200000; i++) printf "x"; print "\";" }' >> src/lib.rs
commit_head "Add a long constant"
