#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn total(prices: &[u32]) -> u32 {
    let mut t = 0;
    for p in prices {
        t += p;
    }
    t
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn total(prices: &[u32]) -> u32 {
    let mut sum = 0;
    for price in prices {
        sum += price;
    }
    sum
}
GITCKO
commit_head "Use clearer names in total"
