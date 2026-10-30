#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod big;

pub fn one() -> u32 {
    big::C1
}
GITCKO
awk 'BEGIN { for (i = 0; i < 40000; i++) printf "pub const C%d: u32 = %d;\n", i, i }' > src/big.rs
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod big;

pub fn one() -> u32 {
    big::C1 + 0
}
GITCKO
commit_head "Touch one"
