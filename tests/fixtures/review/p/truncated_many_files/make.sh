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
for i in $(seq 1 120); do printf 'pub fn f() -> u32 {\n    %s\n}\n' "$i" > "src/m$i.rs"; done
commit_head "Add many modules"
