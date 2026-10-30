#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
#[derive(Default)]
pub struct Options {
    pub verbose: bool,
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
#[derive(Default)]
pub struct Options {
    pub verbose: bool,
    pub color: bool,
}
GITCKO
commit_head "Add a color option"
