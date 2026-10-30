#!/usr/bin/env bash
set -euo pipefail
. "${0%[/\\]*}/../../lib.sh"

start_crate
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct Session {
    pub expires_at: u64,
}

impl Session {
    pub fn is_expired(&self, now: u64) -> bool {
        now >= self.expires_at
    }
}

pub fn allow(session: &Session, now: u64) -> bool {
    !session.is_expired(now)
}
GITCKO
commit_base
mkdir -p src
cat > src/lib.rs <<'GITCKO'
pub struct Session {
    pub expires_at: u64,
}

impl Session {
    pub fn is_expired(&self, now: u64) -> bool {
        now <= self.expires_at
    }
}

pub fn allow(session: &Session, now: u64) -> bool {
    !session.is_expired(now)
}
GITCKO
commit_head "Tidy the expiry check"
