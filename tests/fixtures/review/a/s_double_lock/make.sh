#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::sync::Mutex;

pub struct Counter {
    value: Mutex<u64>,
}

impl Counter {
    pub fn get(&self) -> u64 {
        *self.value.lock().unwrap()
    }

    pub fn bump(&self) -> u64 {
        let mut v = self.value.lock().unwrap();
        *v += 1;
        *v
    }
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
use std::sync::Mutex;

pub struct Counter {
    value: Mutex<u64>,
}

impl Counter {
    pub fn get(&self) -> u64 {
        *self.value.lock().unwrap()
    }

    pub fn bump(&self) -> u64 {
        let mut v = self.value.lock().unwrap();
        *v += 1;
        self.get()
    }
}
GITCKO
commit_head "Reuse get in bump"
