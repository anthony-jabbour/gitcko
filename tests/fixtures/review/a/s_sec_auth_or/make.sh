#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct User {
    pub active: bool,
    pub admin: bool,
}

pub fn can_delete(user: &User) -> bool {
    user.active && user.admin
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct User {
    pub active: bool,
    pub admin: bool,
}

pub fn can_delete(user: &User) -> bool {
    user.active || user.admin
}
GITCKO
commit_head "Rewrite can_delete"
