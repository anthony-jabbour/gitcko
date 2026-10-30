#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod Name;
GITCKO
mkdir -p src
cat > src/Name.rs <<'GITCKO'
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}
GITCKO
commit_base
git mv src/Name.rs src/name_tmp.rs
git mv src/name_tmp.rs src/name.rs
printf 'pub mod name;\n' > src/lib.rs
commit_head "Lower-case the module file"
