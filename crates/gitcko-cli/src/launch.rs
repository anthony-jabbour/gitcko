// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bare `gitcko` on a terminal. The binary supplies [`LiveTui`]; tests supply a
//! session that records the call, so a pipe never opens a full-screen UI.

use std::collections::BTreeMap;
use std::path::Path;

/// What bare `gitcko` runs when both standard streams are a terminal.
pub trait TuiSession: Send + Sync {
    /// Run the UI. `0` is a clean quit.
    fn run(&self, cwd: &Path, env: &BTreeMap<String, String>) -> u8;
}

/// The session the `gitcko` binary uses.
pub struct LiveTui;

impl TuiSession for LiveTui {
    fn run(&self, cwd: &Path, env: &BTreeMap<String, String>) -> u8 {
        match gitcko_tui::run(cwd, env) {
            Ok(code) => code,
            Err(_) => {
                eprintln!("gitcko: could not open the terminal");
                1
            }
        }
    }
}
