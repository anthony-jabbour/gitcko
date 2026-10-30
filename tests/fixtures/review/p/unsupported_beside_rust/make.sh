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
mkdir -p tools
cat > tools/gen.py <<'GITCKO'
x = 1
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

pub fn sub(a: u32, b: u32) -> u32 {
    a.saturating_sub(b)
}
GITCKO
mkdir -p tools
cat > tools/gen.py <<'GITCKO'
x = 2
GITCKO
commit_head "Add sub and bump the generator"
