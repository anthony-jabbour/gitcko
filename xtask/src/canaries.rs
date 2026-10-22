// SPDX-License-Identifier: AGPL-3.0-or-later

//! Redaction canaries (OBSERVABILITY §6, TESTING §10): fake secrets from
//! `gitcko_redact::canaries()` pushed through each sink must never come out. Every
//! such test has `canary` in its name, in whichever crate owns the sink.

use crate::Result;

pub fn check() -> Result {
    // Default profile, not `ci`: the `ci` profile writes target/nextest/ci/junit.xml,
    // and this run would overwrite the full test report CI uploads.
    // `--no-tests=fail`: if a rename ever drops every canary test, this fails instead
    // of passing with nothing checked (TESTING §1, no silent skips).
    crate::run(
        "cargo",
        &[
            "nextest",
            "run",
            "--workspace",
            "--no-tests=fail",
            "-E",
            "test(canary)",
        ],
    )
}
