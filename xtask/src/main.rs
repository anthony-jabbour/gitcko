// SPDX-License-Identifier: AGPL-3.0-or-later

//! `cargo xtask`: Gitcko's dev tasks, as plain Rust instead of shell scripts
//! (the pattern from https://github.com/matklad/cargo-xtask).

mod fuzz;
mod layers;
mod leak_guard;
mod schema;
mod spdx;

use std::process::{Command, ExitCode};

/// Tool versions `setup` installs, so every machine and CI run the same ones.
const NEXTEST_VERSION: &str = "0.9.149";
const DENY_VERSION: &str = "0.20.2";
const INSTA_VERSION: &str = "1.49.0";
const FUZZ_VERSION: &str = "0.13.2";

const USAGE: &str = "\
usage: cargo xtask <task>

  setup                        install pinned tools, enable git hooks
  check                        fmt, clippy, tests: the same as CI
  leak-guard [--range A..B]    scan commits for secrets and private content
  leak-guard --self-test       prove the leak guard blocks what it should
  spdx                         check every .rs file's licence header
  fuzz --build                 build the fuzz targets (nightly from fuzz/rust-toolchain.toml)
  fuzz --run <secs>            run each fuzz target for <secs> seconds
  schema-gen                   write integrations/schemas/ from gitcko-protocol
  schema-check                 fail if integrations/schemas/ differs from the types
  layers                       check crates only depend on their own or lower layers";

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // Tasks use repo-relative paths, so run from the workspace root wherever `cargo xtask` was called.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    if let Err(e) = std::env::set_current_dir(&root) {
        eprintln!("cannot enter workspace root {}: {e}", root.display());
        return ExitCode::FAILURE;
    }
    let result = match args.as_slice() {
        ["setup"] => setup(),
        ["check"] => check(),
        ["leak-guard"] => leak_guard::scan("origin/main..HEAD"),
        ["leak-guard", "--range", range] => leak_guard::scan(range),
        ["leak-guard", "--self-test"] => leak_guard::self_test(),
        // git also passes the remote name ($1); only the URL matters.
        ["leak-guard", "--pre-push", _remote, url] => leak_guard::pre_push(url),
        ["spdx"] => spdx::check(),
        ["fuzz", "--build"] => fuzz::build(),
        ["fuzz", "--run", seconds] => fuzz::run_for(seconds),
        ["schema-gen"] => schema::generate(),
        ["schema-check"] => schema::check(),
        ["layers"] => layers::check(),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn setup() -> Result {
    for (tool, version) in [
        ("cargo-nextest", NEXTEST_VERSION),
        ("cargo-deny", DENY_VERSION),
        ("cargo-insta", INSTA_VERSION),
        ("cargo-fuzz", FUZZ_VERSION),
    ] {
        let subcommand = tool.trim_start_matches("cargo-");
        let installed = output("cargo", &[subcommand, "--version"]).unwrap_or_default();
        if installed.contains(version) {
            println!("{tool} {version} already installed");
        } else {
            run(
                "cargo",
                &["install", "--locked", &format!("{tool}@{version}")],
            )?;
        }
    }
    // The nightly only fuzz/ uses (cargo-fuzz needs sanitizers).
    let nightly = fuzz::toolchain()?;
    run(
        "rustup",
        &[
            "toolchain",
            "install",
            &nightly[1..],
            "--profile",
            "minimal",
        ],
    )?;
    if output("gitleaks", &["version"]).is_err() {
        // gitleaks is a Go binary, so cargo can't install it.
        println!(
            "gitleaks not found; install it for the pre-push leak guard:\n  \
             macOS: brew install gitleaks\n  Windows: winget install Gitleaks.Gitleaks"
        );
    }
    run("git", &["config", "core.hooksPath", ".githooks"])?;
    println!("git hooks enabled (.githooks)");
    Ok(())
}

/// The first three rows of the PR gate, exactly as CI runs them.
fn check() -> Result {
    run("cargo", &["fmt", "--check"])?;
    run(
        "cargo",
        &["clippy", "--all-targets", "--", "-D", "warnings"],
    )?;
    run("cargo", &["nextest", "run", "--profile", "ci"])
}

/// Runs a command with inherited output and fails if it fails.
fn run(program: &str, args: &[&str]) -> Result {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| format!("could not start {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program} {}` failed", args.join(" ")))
    }
}

/// Runs a command and returns its stdout, failing if it fails.
fn output(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not start {program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
