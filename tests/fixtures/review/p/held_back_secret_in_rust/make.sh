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
commit_base
prefix="AK"; prefix="${prefix}IA"
printf 'pub const KEY: &str = "%s%s";\n' "$prefix" "ABCDEFGHIJKLMNOP" > src/keys.rs
commit_head "Update settings"
