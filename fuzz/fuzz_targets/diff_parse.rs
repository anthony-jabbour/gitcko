// SPDX-License-Identifier: AGPL-3.0-or-later

//! Any text the parser accepts must render back to diff text that parses to the same
//! value, and line mapping must never panic.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(changes) = gitcko_diff::parse(text) else {
        return;
    };
    let rendered = changes.to_string();
    let again = gitcko_diff::parse(&rendered).expect("rendered diff parses");
    assert_eq!(again, changes, "render/parse round trip changed the value");
    for file in &changes.files {
        for line in 0..64 {
            let _ = file.old_line(line);
            let _ = file.new_line(line);
        }
    }
});
