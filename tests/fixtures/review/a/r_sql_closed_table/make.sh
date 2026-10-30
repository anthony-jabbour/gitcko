#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
#[derive(Clone, Copy)]
pub enum Table {
    Users,
    Orders,
}

pub fn count(table: Table) -> String {
    match table {
        Table::Users => "SELECT count(*) FROM users".to_string(),
        Table::Orders => "SELECT count(*) FROM orders".to_string(),
    }
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
#[derive(Clone, Copy)]
pub enum Table {
    Users,
    Orders,
}

pub fn count(table: Table) -> String {
    let name = match table {
        Table::Users => "users",
        Table::Orders => "orders",
    };
    format!("SELECT count(*) FROM {name}")
}
GITCKO
commit_head "Build the count query from the table name"
