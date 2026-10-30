#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}
GITCKO
mkdir -p src
cat > src/App.java <<'GITCKO'
class App {
  int add(int a, int b) { return a + b; }
}
GITCKO
commit_base
mkdir -p src
cat > src/App.java <<'GITCKO'
class App {
  int add(int a, int b) { return a + b; }
  int sub(int a, int b) { return a - b; }
}
GITCKO
commit_head "Update src/App.java"
