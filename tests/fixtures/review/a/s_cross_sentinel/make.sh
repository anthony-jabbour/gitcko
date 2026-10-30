#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod use_it;

pub fn position(xs: &[u32], x: u32) -> Option<usize> {
    xs.iter().position(|v| *v == x)
}
GITCKO
mkdir -p src
cat > src/use_it.rs <<'GITCKO'
use crate::position;

pub fn contains(xs: &[u32], x: u32) -> bool {
    position(xs, x).is_some()
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod use_it;

pub fn position(xs: &[u32], x: u32) -> Option<usize> {
    Some(xs.iter().position(|v| *v == x).unwrap_or(0))
}
GITCKO
commit_head "Default position to the start"
