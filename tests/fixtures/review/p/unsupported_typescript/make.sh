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
mkdir -p web
cat > web/app.ts <<'GITCKO'
export const add = (a: number, b: number) => a + b;
GITCKO
commit_base
mkdir -p web
cat > web/app.ts <<'GITCKO'
export const add = (a: number, b: number) => a + b;
export const sub = (a: number, b: number) => a - b;
GITCKO
commit_head "Update web/app.ts"
