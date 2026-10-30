#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

# Constants far enough apart that each change gets its own hunk.
spaced() {
    awk -v offset="$1" 'BEGIN { for (i = 0; i < 600; i++) printf "pub const A%d: u32 = %d;\n\n\n\n\n\n\n\n", i, i + offset }' > src/table.rs
}

start_crate
printf 'pub mod table;\n' > src/lib.rs
spaced 0
commit_base
spaced 1
commit_head "Shift every constant"
