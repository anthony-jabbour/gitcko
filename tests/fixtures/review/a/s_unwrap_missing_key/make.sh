#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::collections::HashMap;

pub fn port(config: &HashMap<String, String>) -> u16 {
    config.get("port").and_then(|p| p.parse().ok()).unwrap_or(8080)
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::collections::HashMap;

pub fn port(config: &HashMap<String, String>) -> u16 {
    config.get("port").unwrap().parse().unwrap()
}
GITCKO
commit_head "Read the port directly"
