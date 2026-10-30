#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn is_small(n: usize) -> bool {
    n < 1024
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
const SMALL: usize = 1024;

pub fn is_small(n: usize) -> bool {
    n < SMALL
}
GITCKO
commit_head "Name the small size limit"
