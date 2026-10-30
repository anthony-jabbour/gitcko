#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod use_it;

#[derive(Clone, Copy)]
pub enum Status {
    Ok,
    Busy,
    Failed,
}
GITCKO
mkdir -p src
cat > src/use_it.rs <<'GITCKO'
use crate::Status;

/// The byte sent to peers; existing peers already store these values.
pub fn wire(status: Status) -> u8 {
    status as u8
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod use_it;

#[derive(Clone, Copy)]
pub enum Status {
    Ok,
    Failed,
    Busy,
}
GITCKO
commit_head "Sort status variants by severity"
