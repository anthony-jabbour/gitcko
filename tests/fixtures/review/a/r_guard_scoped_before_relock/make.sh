#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::sync::Mutex;

pub fn bump(value: &Mutex<u64>) -> u64 {
    let mut v = value.lock().unwrap();
    *v += 1;
    *v
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::sync::Mutex;

pub fn bump(value: &Mutex<u64>) -> u64 {
    {
        let mut v = value.lock().unwrap();
        *v += 1;
    }
    *value.lock().unwrap()
}
GITCKO
commit_head "Release the lock before reading back"
