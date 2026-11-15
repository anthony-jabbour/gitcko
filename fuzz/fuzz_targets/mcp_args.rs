// SPDX-License-Identifier: AGPL-3.0-or-later

//! Any bytes as MCP tool arguments: must never panic when checked against every tool.

#![no_main]

use gitcko_mcp::check_mcp_args;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    check_mcp_args(data);
});