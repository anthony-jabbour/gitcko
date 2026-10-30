#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod use_it;

/// The request timeout in seconds.
pub fn timeout() -> u64 {
    30
}
GITCKO
mkdir -p src
cat > src/use_it.rs <<'GITCKO'
use crate::timeout;

pub fn deadline(now_secs: u64) -> u64 {
    now_secs + timeout()
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod use_it;

/// The request timeout in milliseconds.
pub fn timeout() -> u64 {
    30_000
}
GITCKO
commit_head "Express the timeout in milliseconds"
