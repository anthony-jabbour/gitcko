#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::io::{self, Write};

pub fn save(out: &mut impl Write, record: &[u8]) -> io::Result<()> {
    out.write_all(record)?;
    out.flush()
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::io::{self, Write};

pub fn save(out: &mut impl Write, record: &[u8]) -> io::Result<()> {
    let _ = out.write_all(record);
    out.flush()
}
GITCKO
commit_head "Keep going after short writes"
