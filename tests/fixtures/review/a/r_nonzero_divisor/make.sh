#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::num::NonZeroU32;

pub fn share(total: u32, parts: NonZeroU32) -> u32 {
    total.checked_div(parts.get()).unwrap_or(0)
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::num::NonZeroU32;

pub fn share(total: u32, parts: NonZeroU32) -> u32 {
    total / parts.get()
}
GITCKO
commit_head "Drop the redundant checked division"
