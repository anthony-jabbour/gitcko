// SPDX-License-Identifier: AGPL-3.0-or-later

//! Process entry for the `gitcko` binary. All behaviour lives in the library so tests
//! can call it without spawning a process.

fn main() {
    let code = gitcko_cli::run_from_env();
    std::process::exit(code.into());
}
