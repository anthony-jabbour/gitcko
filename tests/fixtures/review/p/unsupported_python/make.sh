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
mkdir -p app
cat > app/main.py <<'GITCKO'
def add(a, b):
    return a + b
GITCKO
commit_base
mkdir -p app
cat > app/main.py <<'GITCKO'
def add(a, b):
    return a + b

def sub(a, b):
    return a - b
GITCKO
commit_head "Update app/main.py"
