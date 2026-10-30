#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
/// Mean of the samples, or 0 when there are none.
pub fn average(samples: &[u32]) -> u32 {
    if samples.is_empty() {
        return 0;
    }
    samples.iter().sum::<u32>() / samples.len() as u32
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
/// Mean of the samples, or 0 when there are none.
pub fn average(samples: &[u32]) -> u32 {
    let total: u32 = samples.iter().sum();
    total / samples.len() as u32
}
GITCKO
mkdir -p .gitcko
cat > .gitcko/config.toml <<'GITCKO'
schema_version = 1

[review]
comment_types = ["style"]
GITCKO
commit_head "Limit comment types"
