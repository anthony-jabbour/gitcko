// SPDX-License-Identifier: AGPL-3.0-or-later

//! A model's tool call as any bytes: the first byte picks a tool name (one past the
//! list is an unknown tool), the rest is the argument text. Parsing must return a call
//! or a `CallError`, never panic, and give the same answer twice.

#![no_main]

use gitcko_tools::{NAMES, ToolCall};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    let name = NAMES.get(pick as usize % (NAMES.len() + 1)).copied().unwrap_or("run_command");
    let args = String::from_utf8_lossy(rest);
    let first = ToolCall::parse(name, &args);
    assert_eq!(first, ToolCall::parse(name, &args));
    if name == "run_command" {
        assert!(first.is_err());
    }
});
