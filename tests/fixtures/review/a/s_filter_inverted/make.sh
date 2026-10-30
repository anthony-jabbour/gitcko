#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct Task {
    pub done: bool,
}

pub fn pending(tasks: Vec<Task>) -> Vec<Task> {
    tasks.into_iter().filter(|t| !t.done).collect()
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct Task {
    pub done: bool,
}

pub fn pending(tasks: Vec<Task>) -> Vec<Task> {
    tasks.into_iter().filter(|t| t.done).collect()
}
GITCKO
commit_head "Simplify pending"
