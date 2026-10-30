#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct Point {
    pub x: i32,
    pub y: i32,
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::fmt;

pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}
GITCKO
commit_head "Display points"
