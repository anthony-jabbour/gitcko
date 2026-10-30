#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct Account {
    pub balance: i64,
}

pub fn transfer(from: &mut Account, to: &mut Account, amount: i64) {
    from.balance -= amount;
    to.balance += amount;
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct Account {
    pub balance: i64,
}

pub fn transfer(from: &mut Account, to: &mut Account, amount: i64) {
    to.balance -= amount;
    from.balance += amount;
}
GITCKO
commit_head "Reorder transfer updates"
