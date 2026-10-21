// SPDX-License-Identifier: AGPL-3.0-or-later

//! Every Rust file starts with the SPDX licence line for its folder.

use crate::{Result, output};

/// Folders under Apache-2.0; everything else is AGPL-3.0-or-later.
const APACHE_PREFIXES: &[&str] = &[
    "integrations/",
    "crates/gitcko-protocol/",
    "crates/gitcko-config-import/",
];

fn expected_license(path: &str) -> &'static str {
    if APACHE_PREFIXES.iter().any(|p| path.starts_with(p)) {
        "Apache-2.0"
    } else {
        "AGPL-3.0-or-later"
    }
}

/// Returns an error message if `first_line` isn't the right header for `path`.
fn header_problem(path: &str, first_line: &str) -> Option<String> {
    let want = format!("// SPDX-License-Identifier: {}", expected_license(path));
    (first_line.trim_end() != want).then(|| format!("{path}: first line must be `{want}`"))
}

pub fn check() -> Result {
    // Tracked and staged files only, so build output and scratch files are skipped.
    let files = output("git", &["ls-files", "--cached", "--", "*.rs"])?;
    let mut problems = Vec::new();
    for path in files.lines() {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        problems.extend(header_problem(path, text.lines().next().unwrap_or("")));
    }
    if problems.is_empty() {
        println!("spdx: {} files ok", files.lines().count());
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_files_are_agpl() {
        assert_eq!(
            header_problem(
                "xtask/src/main.rs",
                "// SPDX-License-Identifier: AGPL-3.0-or-later"
            ),
            None
        );
    }

    #[test]
    fn integrations_and_protocol_are_apache() {
        for path in ["integrations/npm/x.rs", "crates/gitcko-protocol/src/lib.rs"] {
            assert_eq!(
                header_problem(path, "// SPDX-License-Identifier: Apache-2.0"),
                None
            );
            assert!(
                header_problem(path, "// SPDX-License-Identifier: AGPL-3.0-or-later").is_some()
            );
        }
    }

    #[test]
    fn missing_or_wrong_header_fails() {
        assert!(header_problem("crates/gitcko-git/src/lib.rs", "").is_some());
        assert!(header_problem("crates/gitcko-git/src/lib.rs", "//! docs").is_some());
        assert!(
            header_problem(
                "crates/gitcko-git/src/lib.rs",
                "// SPDX-License-Identifier: MIT"
            )
            .is_some()
        );
    }

    #[test]
    fn similar_crate_name_is_not_apache() {
        assert!(
            header_problem(
                "crates/gitcko-protocol-extra/src/lib.rs",
                "// SPDX-License-Identifier: Apache-2.0"
            )
            .is_some()
        );
    }
}
