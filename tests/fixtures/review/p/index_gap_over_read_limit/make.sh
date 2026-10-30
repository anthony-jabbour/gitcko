#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod big;
GITCKO
mkdir -p src
cat > src/big.rs <<'GITCKO'
pub const C0: u32 = 0;
GITCKO
commit_base
awk 'BEGIN { for (i = 0; i < 170000; i++) printf "pub const C%d: u32 = %d;\n", i, i }' > src/big.rs
commit_head "Grow the generated table"
