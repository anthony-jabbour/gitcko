// SPDX-License-Identifier: AGPL-3.0-or-later

//! The command tree and the version line. Later tests cover what `review` does.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use gitcko_cli::{Io, RunEnv, TuiSession, execute_at, version_line};

fn run(args: &[&str]) -> (u8, String, String) {
    let io = Io::capture();
    // An empty config and a temp cache, so a parsed `review` flag cannot reach the
    // user's models or start a run while these tests only check the command tree.
    let dir = std::env::temp_dir().join(format!("gitcko-cli-tree-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let env = BTreeMap::from([
        (
            "GITCKO_CONFIG".into(),
            dir.join("missing.toml").display().to_string(),
        ),
        ("GITCKO_CACHE_DIR".into(), dir.display().to_string()),
    ]);
    let cwd = std::env::current_dir().expect("cwd");
    let code = execute_at(
        args,
        &RunEnv {
            cwd: &cwd,
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
            tui: None,
            keychain: None,
            answers: None,
        },
        &io,
    );
    (code, io.stdout_text(), io.stderr_text())
}

fn expected_version() -> String {
    format!(
        "gitcko {} (engine {}, result schema gitcko.review/1, mcp gitcko.mcp/1)\n",
        env!("CARGO_PKG_VERSION"),
        gitcko_review::VERSION
    )
}

#[test]
fn version_line_names_the_binary_engine_and_schemas() {
    assert_eq!(version_line() + "\n", expected_version());
    let (code, out, err) = run(&["version"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, expected_version());
    assert!(err.is_empty(), "{err}");
    let (flag_code, flag_out, _) = run(&["--version"]);
    assert_eq!(flag_code, 0);
    assert_eq!(flag_out, expected_version());
}

#[test]
fn review_flags_cannot_mix_with_show_subcommand() {
    let (code, out, err) = run(&["review", "--json", "show", "rv_01AAAAAAAAAAAAAA0000000000"]);
    assert_eq!(code, 2, "stdout={out} stderr={err}");
    assert!(out.is_empty(), "{out}");
    assert!(!err.is_empty(), "clap names the conflict");
}

#[test]
fn a_version_command_with_an_extra_argument_is_rejected() {
    let (code, out, err) = run(&["version", "extra"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(out.is_empty(), "{out}");
    assert!(!err.is_empty(), "a bad invocation names the problem");
}

#[test]
fn bare_gitcko_writes_help_to_stderr_and_exits_2() {
    let (code, out, err) = run(&[]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(out.is_empty(), "help is not success output: {out}");
    assert!(err.contains("Usage:"), "{err}");
    assert!(err.contains("review"), "{err}");
    assert!(err.contains("version"), "{err}");
}

#[test]
fn help_on_stdout_exits_0_which_is_the_control_for_bare_gitcko() {
    let (code, out, err) = run(&["--help"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("Usage:"), "{out}");
    assert!(out.contains("review"), "{out}");
    assert!(err.is_empty(), "{err}");
}

#[test]
fn the_review_tree_accepts_its_flags_and_rejects_unknown_ones() {
    let (code, out, err) = run(&["review", "--help"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("--json"), "{out}");
    assert!(out.contains("--fail-on"), "{out}");
    assert!(out.contains("--paths"), "{out}");
    assert!(out.contains("--include"), "{out}");
    assert!(out.contains("show"), "{out}");
    assert!(out.contains("status"), "{out}");

    let (bad, _, err) = run(&["review", "--nope"]);
    assert_eq!(bad, 2);
    assert!(
        err.contains("unexpected") || err.contains("--nope"),
        "{err}"
    );

    let (known, _, err) = run(&["review", "--plus"]);
    assert!(
        !err.contains("unexpected argument"),
        "--plus is a real flag: {known} {err}"
    );
}

#[test]
fn bad_fail_on_and_layout_values_exit_2_and_the_real_values_parse() {
    let (bad, _, err) = run(&["review", "--fail-on", "P3"]);
    assert_eq!(bad, 2, "{err}");
    assert!(!err.is_empty(), "{err}");

    let (ok, _, err) = run(&["review", "--fail-on", "P0", "--layout", "comments"]);
    assert!(
        !err.contains("invalid value"),
        "P0 and comments are accepted: {ok} {err}"
    );

    let (diff, _, err) = run(&["review", "--layout", "diff"]);
    assert!(
        !err.contains("invalid value"),
        "diff parses so the command can refuse it later: {diff} {err}"
    );
    let (side, _, err) = run(&["review", "--layout", "side"]);
    assert_eq!(side, 2);
    assert!(err.contains("invalid") || err.contains("side"), "{err}");
}

#[test]
fn context_outside_0_to_60_exits_2_and_15_is_accepted() {
    let (bad, _, err) = run(&["review", "--context", "61"]);
    assert_eq!(bad, 2, "{err}");
    let (ok, _, err) = run(&["review", "--context", "15"]);
    assert!(
        !err.contains("61") && !err.to_lowercase().contains("invalid"),
        "15 is inside the range: {ok} {err}"
    );
}

#[test]
fn index_and_cache_are_in_the_tree() {
    let (code, out, err) = run(&["index", "--help"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("--full"), "{out}");

    let (code, out, err) = run(&["cache", "--help"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("stats"), "{out}");
    assert!(out.contains("purge"), "{out}");

    let (code, out, err) = run(&["cache", "purge", "--help"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("--repo"), "{out}");
    assert!(out.contains("--all"), "{out}");
    assert!(out.contains("-f"), "{out}");

    let (code, _, err) = run(&["cache", "purge"]);
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = run(&["cache", "purge", "--repo", "--all"]);
    assert_eq!(code, 2, "repo and all are exclusive: {err}");
    let (code, _, err) = run(&["cache", "stats"]);
    assert_eq!(code, 2);
    assert!(
        !err.contains("unexpected"),
        "stats is a real subcommand: {err}"
    );
}

#[test]
fn the_binary_version_and_bare_help_match_the_library() {
    let mut version = bin();
    version.arg("version");
    let version = version.output().expect("run gitcko");
    assert!(version.status.success(), "{version:?}");
    assert_eq!(
        String::from_utf8(version.stdout).unwrap(),
        expected_version()
    );
    assert!(version.stderr.is_empty());

    let mut bare = bin();
    let bare = bare.output().expect("run gitcko");
    assert_eq!(bare.status.code(), Some(2), "{bare:?}");
    assert!(bare.stdout.is_empty(), "non-tty help stays off stdout");
    let err = String::from_utf8(bare.stderr).unwrap();
    assert!(err.contains("Usage:"), "{err}");
}

struct RecordingTui(AtomicBool);

impl TuiSession for RecordingTui {
    fn run(&self, _cwd: &std::path::Path, _env: &BTreeMap<String, String>) -> u8 {
        self.0.store(true, Ordering::SeqCst);
        0
    }
}

#[test]
fn bare_gitcko_on_a_tty_opens_the_tui_and_a_pipe_stays_on_help() {
    let dir = std::env::temp_dir();
    let env = BTreeMap::new();
    let session = RecordingTui(AtomicBool::new(false));
    let io = Io::capture();
    let code = execute_at(
        &[],
        &RunEnv {
            cwd: &dir,
            env: &env,
            runner: None,
            stdout_is_tty: true,
            stdin_is_tty: true,
            tui: Some(&session),
        },
        &io,
    );
    assert_eq!(code, 0, "{}", io.stderr_text());
    assert!(session.0.load(Ordering::SeqCst), "the TUI session ran");
    assert!(
        !io.stderr_text().contains("Usage:"),
        "a TTY does not dump help: {}",
        io.stderr_text()
    );

    let session = RecordingTui(AtomicBool::new(false));
    let io = Io::capture();
    let code = execute_at(
        &[],
        &RunEnv {
            cwd: &dir,
            env: &env,
            runner: None,
            stdout_is_tty: true,
            stdin_is_tty: false,
            tui: Some(&session),
        },
        &io,
    );
    assert_eq!(code, 2, "stdout alone is not a TUI session");
    assert!(
        !session.0.load(Ordering::SeqCst),
        "the control does not launch"
    );
    assert!(io.stderr_text().contains("Usage:"), "{}", io.stderr_text());
    assert!(io.stdout_text().is_empty(), "{}", io.stdout_text());
}

#[cfg(unix)]
#[test]
fn non_utf8_environment_values_do_not_panic() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let mut cmd = bin();
    cmd.env(
        OsStr::from_bytes(b"GITCKO_\xff"),
        OsStr::from_bytes(b"\xff\xff"),
    );
    cmd.arg("version");
    let out = cmd.output().expect("run gitcko version");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8(out.stdout).unwrap(), expected_version());
}

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gitcko"));
    cmd.env_clear();
    for key in [
        "PATH",
        "Path",
        "PATHEXT",
        "SystemRoot",
        "HOME",
        "USERPROFILE",
    ] {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    let _ = cmd.stdin(std::process::Stdio::null());
    let _ = Write::flush(&mut std::io::stdout());
    cmd
}
