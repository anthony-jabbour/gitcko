// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `gitcko` binary's behaviour, separate from `main` so tests can run a command
//! without a process. The binary's only job is to own the terminal and the runtime.

mod cli;

use std::io::{self, Write};

use clap::{CommandFactory, FromArgMatches};

use crate::cli::Cli;

/// Where a command writes. Tests pass buffers; the binary passes the real terminal.
pub struct Io<'a> {
    /// Review JSON and the version line. Progress never goes here.
    pub stdout: &'a mut dyn Write,
    /// Help (when there is no command), errors, and progress.
    pub stderr: &'a mut dyn Write,
}

/// The version line (P5_PLAN §10.2). The binary and engine versions come from their
/// crates; the schema ids are the names this build writes, including MCP's, which has
/// no crate of its own yet.
pub fn version_line() -> String {
    format!(
        "gitcko {} (engine {}, result schema gitcko.review/1, mcp gitcko.mcp/1)",
        env!("CARGO_PKG_VERSION"),
        gitcko_review::VERSION
    )
}

/// Run `gitcko` with the process arguments, environment and working directory.
pub fn run_from_env() -> u8 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    let mut io = Io {
        stdout: &mut stdout,
        stderr: &mut stderr,
    };
    execute(&refs, &mut io)
}

/// Run one invocation. `args` does not include the program name.
///
/// A bare `gitcko` writes help to stderr and returns 2, because there is no TUI in
/// this build and a pipe must not look like success. `--help` stays on stdout and
/// returns 0, which is clap's own split
/// (<https://docs.rs/clap/4.6.7/clap/error/struct.Error.html#method.exit_code>).
pub fn execute(args: &[&str], io: &mut Io<'_>) -> u8 {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push("gitcko".to_string());
    argv.extend(args.iter().map(|s| (*s).to_string()));
    let command = Cli::command();
    let matches = match command.try_get_matches_from(&argv) {
        Ok(matches) => matches,
        Err(err) => return render_clap(err, io),
    };
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(err) => return render_clap(err, io),
    };
    if cli.version_flag {
        return write_version(io);
    }
    match cli.command {
        Some(cli::Command::Version) => write_version(io),
        Some(cli::Command::Review(_) | cli::Command::Index(_) | cli::Command::Cache(_)) => {
            // The commands parse in this commit. What they do lands in the next ones.
            let _ = writeln!(io.stderr, "not available in this build");
            2
        }
        None => {
            let mut command = Cli::command();
            let mut help = Vec::new();
            let _ = command.write_long_help(&mut help);
            let _ = io.stderr.write_all(&help);
            let _ = writeln!(io.stderr);
            2
        }
    }
}

fn write_version(io: &mut Io<'_>) -> u8 {
    let _ = writeln!(io.stdout, "{}", version_line());
    0
}

fn render_clap(err: clap::Error, io: &mut Io<'_>) -> u8 {
    let text = err.render();
    let code = err.exit_code();
    let writer: &mut dyn Write = if code == 0 { io.stdout } else { io.stderr };
    let _ = write!(writer, "{text}");
    u8::try_from(code).unwrap_or(2)
}
