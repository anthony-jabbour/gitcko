#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn with_retries(mut attempt: impl FnMut() -> Result<u32, ()>, max: u32) -> Result<u32, ()> {
    let mut tries = 0;
    loop {
        match attempt() {
            Ok(v) => return Ok(v),
            Err(()) if tries + 1 >= max => return Err(()),
            Err(()) => tries += 1,
        }
    }
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn with_retries(mut attempt: impl FnMut() -> Result<u32, ()>, max: u32) -> Result<u32, ()> {
    let tries = 0;
    loop {
        match attempt() {
            Ok(v) => return Ok(v),
            Err(()) if tries + 1 >= max => return Err(()),
            Err(()) => continue,
        }
    }
}
GITCKO
commit_head "Clean up the retry loop"
