# Sourced by each case's make.sh. Fixture repos: branch `base`, then `main` is the head.

# Starts a repository with an empty crate.
start_crate() {
    git init -q -b main
    mkdir -p src
    printf '[package]\nname = "fixture"\nversion = "0.1.0"\nedition = "2021"\n' > Cargo.toml
    : > src/lib.rs
}

commit_base() {
    git add -A
    git commit -q -m "Base"
    git branch base
}

commit_head() {
    git add -A
    git commit -q -m "$1"
}

# Suite F's crate: every fault runs over this one change, whose bug is on line 3.
fault_crate() {
    start_crate
    cat > src/lib.rs <<'GITCKO'
pub fn average(samples: &[u32]) -> u32 {
    if samples.is_empty() {
        return 0;
    }
    samples.iter().sum::<u32>() / samples.len() as u32
}
GITCKO
    commit_base
    cat > src/lib.rs <<'GITCKO'
pub fn average(samples: &[u32]) -> u32 {
    let total: u32 = samples.iter().sum();
    total / samples.len() as u32
}
GITCKO
    commit_head "Simplify average"
}
