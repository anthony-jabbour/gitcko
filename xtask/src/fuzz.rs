// SPDX-License-Identifier: AGPL-3.0-or-later

//! `cargo xtask fuzz --build | --run <secs>` for the cargo-fuzz targets in `fuzz/`
//! (TESTING §10): `--build` on PRs that touch a parser, `--run` nightly and on external
//! PRs.

use crate::{Result, output, run};

/// Seed inputs per target, so fuzzing starts from real examples instead of nothing.
fn seeds(target: &str) -> Option<&'static str> {
    match target {
        "diff_parse" => Some("crates/gitcko-diff/tests/data"),
        _ => None,
    }
}

/// The nightly pinned in `fuzz/rust-toolchain.toml`, as `+nightly-…` for cargo.
pub fn toolchain() -> Result<String> {
    let file = std::fs::read_to_string("fuzz/rust-toolchain.toml")
        .map_err(|e| format!("fuzz/rust-toolchain.toml: {e}"))?;
    channel(&file)
        .map(|c| format!("+{c}"))
        .ok_or_else(|| "fuzz/rust-toolchain.toml has no `channel = \"…\"` line".into())
}

fn channel(toml: &str) -> Option<&str> {
    toml.lines()
        .find_map(|line| line.trim().strip_prefix("channel"))
        .and_then(|rest| rest.trim().strip_prefix('='))
        .map(|value| value.trim().trim_matches('"'))
        .filter(|value| !value.is_empty())
}

pub fn build() -> Result {
    run("cargo", &[&toolchain()?, "fuzz", "build"])
}

/// Runs every target for `seconds` each, starting from its seeds; a crash fails the task
/// and cargo-fuzz leaves the input under `fuzz/artifacts/`.
pub fn run_for(seconds: &str) -> Result {
    let seconds: u64 = seconds
        .parse()
        .map_err(|_| format!("not a number of seconds: {seconds}"))?;
    let toolchain = toolchain()?;
    let targets = output("cargo", &[&toolchain, "fuzz", "list"])?;
    for target in targets.lines().map(str::trim).filter(|t| !t.is_empty()) {
        let corpus = format!("fuzz/corpus/{target}");
        std::fs::create_dir_all(&corpus).map_err(|e| format!("{corpus}: {e}"))?;
        let time = format!("-max_total_time={seconds}");
        let mut args = vec![toolchain.as_str(), "fuzz", "run", target, corpus.as_str()];
        args.extend(seeds(target));
        args.extend(["--", time.as_str()]);
        println!("fuzz: {target} for {seconds}s");
        run("cargo", &args)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::channel;

    #[test]
    fn reads_the_pinned_channel() {
        let file =
            "# comment\n[toolchain]\nchannel = \"nightly-2026-10-15\"\nprofile = \"minimal\"\n";
        assert_eq!(channel(file), Some("nightly-2026-10-15"));
        assert_eq!(channel("[toolchain]\n"), None);
        assert_eq!(channel("channel = \"\""), None);
    }
}
