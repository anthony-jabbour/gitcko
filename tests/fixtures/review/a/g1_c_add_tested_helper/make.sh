#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn is_even(n: u32) -> bool {
    n % 2 == 0
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn is_even(n: u32) -> bool {
    n % 2 == 0
}

pub fn evens(xs: &[u32]) -> Vec<u32> {
    xs.iter().copied().filter(|x| is_even(*x)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_even_numbers() {
        assert_eq!(evens(&[1, 2, 3, 4]), [2, 4]);
    }
}
GITCKO
commit_head "Add evens"
