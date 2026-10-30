#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod a;
GITCKO
mkdir -p src
cat > src/a.rs <<'GITCKO'
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}
GITCKO
commit_base
mkdir -p src/a
git mv src/a.rs src/a/mod.rs
commit_head "Move the module into a folder"
