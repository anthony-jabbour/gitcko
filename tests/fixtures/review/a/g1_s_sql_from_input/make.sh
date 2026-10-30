#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct Query {
    pub text: String,
    pub params: Vec<String>,
}

pub fn find_user(name: &str) -> Query {
    Query {
        text: "SELECT id FROM users WHERE name = ?".to_string(),
        params: vec![name.to_string()],
    }
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct Query {
    pub text: String,
    pub params: Vec<String>,
}

pub fn find_user(name: &str) -> Query {
    Query {
        text: format!("SELECT id FROM users WHERE name = '{name}'"),
        params: Vec::new(),
    }
}
GITCKO
commit_head "Build the user query inline"
