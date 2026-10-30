#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn check(n: u32) -> Result<u32, String> {
    if n > 10 {
        return Err("bad".to_string());
    }
    Ok(n)
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn check(n: u32) -> Result<u32, String> {
    if n > 10 {
        return Err("value is above the limit of 10".to_string());
    }
    Ok(n)
}
GITCKO
commit_head "Explain the check error"
