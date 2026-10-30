#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn label(n: i32) -> &'static str {
    if n < 0 {
        "negative"
    } else {
        if n == 0 { "zero" } else { "positive" }
    }
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn label(n: i32) -> &'static str {
    if n < 0 {
        return "negative";
    }
    if n == 0 { "zero" } else { "positive" }
}
GITCKO
commit_head "Flatten label"
