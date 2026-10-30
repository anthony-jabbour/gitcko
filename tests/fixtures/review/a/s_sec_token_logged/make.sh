#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn login_line(user: &str, _token: &str) -> String {
    format!("login user={user}")
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub fn login_line(user: &str, token: &str) -> String {
    format!("login user={user} token={token}")
}
GITCKO
commit_head "Add detail to the login line"
