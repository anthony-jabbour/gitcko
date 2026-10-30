#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod use_it;

pub struct Size {
    pub width: u32,
    pub height: u32,
}

pub fn size(width: u32, height: u32) -> Size {
    Size { width, height }
}
GITCKO
mkdir -p src
cat > src/use_it.rs <<'GITCKO'
use crate::{Size, size};

pub fn banner() -> Size {
    size(800, 100)
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod use_it;

pub struct Size {
    pub width: u32,
    pub height: u32,
}

pub fn size(height: u32, width: u32) -> Size {
    Size { width, height }
}
GITCKO
commit_head "Take height first"
