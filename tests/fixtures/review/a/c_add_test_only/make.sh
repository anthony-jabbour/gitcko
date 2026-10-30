#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn double(n: u32) -> u32 {
    n * 2
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn double(n: u32) -> u32 {
    n * 2
}

#[cfg(test)]
mod tests {
    #[test]
    fn doubles() {
        assert_eq!(super::double(2), 4);
    }
}
GITCKO
commit_head "Test double"
