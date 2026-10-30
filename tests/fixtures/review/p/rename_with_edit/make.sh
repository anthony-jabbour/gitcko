#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub mod old_name;
GITCKO
mkdir -p src
cat > src/old_name.rs <<'GITCKO'
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}
GITCKO
commit_base
git mv src/old_name.rs src/new_name.rs
printf 'pub mod new_name;\n' > src/lib.rs
printf '\npub fn zero() -> u32 {\n    0\n}\n' >> src/new_name.rs
commit_head "Rename and extend the module"
