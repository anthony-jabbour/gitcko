#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn join(parts: &[&str]) -> String {
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(part);
    }
    out
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn join(parts: &[&str]) -> String {
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate().take(parts.len().saturating_sub(1)) {
        if i > 0 {
            out.push(',');
        }
        out.push_str(part);
    }
    out
}
GITCKO
commit_head "Avoid a trailing comma"
