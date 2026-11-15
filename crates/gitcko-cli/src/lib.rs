// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `gitcko` binary's behaviour, separate from `main` so tests can run a command
//! without a process. The binary's only job is to own the terminal and the runtime.

mod cache;
mod cli;
mod clock;
mod exit;
mod history;
mod review;

use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clap::{CommandFactory, FromArgMatches};

use crate::cli::Cli;
pub use crate::review::{ReviewJob, ReviewRunner, RunError, repo_cache_id};

/// Where a command writes. Tests pass buffers; the binary passes the real terminal.
/// The handles are shared so a progress callback can write while the command runs.
pub struct Io<O, E> {
    /// Review JSON and the version line. Progress never goes here.
    pub stdout: Arc<Mutex<O>>,
    /// Help (when there is no command), errors, and progress.
    pub stderr: Arc<Mutex<E>>,
}

impl Io<Vec<u8>, Vec<u8>> {
    /// Buffers for a test. `stdout_is_tty` is not stored here; pass it on [`RunEnv`].
    pub fn capture() -> Io<Vec<u8>, Vec<u8>> {
        Io {
            stdout: Arc::new(Mutex::new(Vec::new())),
            stderr: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// What was written to stdout, as text.
    pub fn stdout_text(&self) -> String {
        bytes_to_string(&self.stdout)
    }

    /// What was written to stderr, as text.
    pub fn stderr_text(&self) -> String {
        bytes_to_string(&self.stderr)
    }
}

fn bytes_to_string(buf: &Mutex<Vec<u8>>) -> String {
    let bytes = buf.lock().unwrap_or_else(|err| err.into_inner());
    String::from_utf8(bytes.clone()).expect("command output is utf-8")
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

/// What one invocation may see. Tests pass a temp directory, a temp env and a fake
/// runner so a review never calls a model or the user's cache.
pub struct RunEnv<'a> {
    /// Working directory. `review` opens this as the repository.
    pub cwd: &'a Path,
    /// Process environment. `GITCKO_CONFIG` and `GITCKO_CACHE_DIR` must be absolute
    /// when set.
    pub env: &'a BTreeMap<String, String>,
    /// When set, used instead of the real engine.
    pub runner: Option<&'a dyn ReviewRunner>,
    /// True when stdout is a terminal, which selects the boxed layout.
    pub stdout_is_tty: bool,
    /// True when stdin is a terminal. This build still requires `-f` to purge.
    pub stdin_is_tty: bool,
}

/// Run `gitcko` with the process arguments, environment and working directory.
pub fn run_from_env() -> u8 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let io = Io {
        stdout: Arc::new(Mutex::new(StdOut)),
        stderr: Arc::new(Mutex::new(StdErr)),
    };
    let run = RunEnv {
        cwd: &cwd,
        env: &env,
        runner: None,
        stdout_is_tty: io::stdout().is_terminal(),
        stdin_is_tty: io::stdin().is_terminal(),
    };
    execute_at(&refs, &run, &io)
}

/// Run one invocation against the process environment. `args` does not include the
/// program name. Review tests should use [`execute_at`] with a temp env.
pub fn execute(args: &[&str], io: &Io<Vec<u8>, Vec<u8>>) -> u8 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let run = RunEnv {
        cwd: &cwd,
        env: &env,
        runner: None,
        stdout_is_tty: false,
        stdin_is_tty: false,
    };
    execute_at(args, &run, io)
}

/// Run one invocation with an explicit directory, environment and runner.
///
/// A bare `gitcko` writes help to stderr and returns 2, because there is no TUI in
/// this build and a pipe must not look like success. `--help` stays on stdout and
/// returns 0, which is clap's own split
/// (<https://docs.rs/clap/4.6.7/clap/error/struct.Error.html#method.exit_code>).
pub fn execute_at<O, E>(args: &[&str], run: &RunEnv<'_>, io: &Io<O, E>) -> u8
where
    O: Write + Send,
    E: Write + Send,
{
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
        Some(cli::Command::Review(args)) => match args.action {
            Some(cli::ReviewCmd::Show(show)) => {
                history::show(&show, run.cwd, run.env, run.stdout_is_tty, io)
            }
            Some(cli::ReviewCmd::Status(status)) => history::status(&status, run.cwd, run.env, io),
            None => review::run(&args, run.cwd, run.env, run.runner, run.stdout_is_tty, io),
        },
        Some(cli::Command::Index(args)) => cache::index(&args, run.cwd, run.env, io),
        Some(cli::Command::Cache(args)) => {
            cache::cache(&args, run.cwd, run.env, run.stdin_is_tty, io)
        }
        None => {
            let mut command = Cli::command();
            let mut help = Vec::new();
            let _ = command.write_long_help(&mut help);
            let _ = lock(&io.stderr).write_all(&help);
            let _ = writeln!(lock(&io.stderr));
            2
        }
    }
}

fn write_version<O: Write, E>(io: &Io<O, E>) -> u8 {
    let _ = writeln!(lock(&io.stdout), "{}", version_line());
    0
}

fn render_clap<O: Write, E: Write>(err: clap::Error, io: &Io<O, E>) -> u8 {
    let text = err.render();
    let code = err.exit_code();
    if code == 0 {
        let _ = write!(lock(&io.stdout), "{text}");
    } else {
        let _ = write!(lock(&io.stderr), "{text}");
    }
    u8::try_from(code).unwrap_or(2)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

/// Forwards to the process stdout. The binary owns one of these behind a mutex so
/// progress and the result can share it.
struct StdOut;

impl Write for StdOut {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stdout().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

/// Forwards to the process stderr.
struct StdErr;

impl Write for StdErr {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stderr().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}
