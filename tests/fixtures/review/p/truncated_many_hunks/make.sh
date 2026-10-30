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
awk 'BEGIN { for (i = 0; i < 600; i++) printf "pub const A%d: u32 = %d;\n\n\n\n\n\n\n\n", i, i }' >> src/lib.rs
commit_head "Add spaced constants"
