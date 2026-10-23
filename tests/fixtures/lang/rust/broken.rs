// SPDX-License-Identifier: AGPL-3.0-or-later
// Synthetic fixture: syntax errors. The parser still recovers what it can.

pub fn fine() -> u32 {
    1
}

fn broken( -> {
    let x = ;
}

pub struct AfterTheError {
    field: u32,
}
