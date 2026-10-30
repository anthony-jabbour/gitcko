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
mkdir -p assets
printf '\000\001' > assets/logo.bin
commit_base
printf '\000\002' > assets/logo.bin
commit_head "Replace the logo"
