// SPDX-License-Identifier: AGPL-3.0-or-later

//! `gitcko index` and `gitcko cache`. The index is local files under the cache; purge
//! deletes a repository's directory, reviews included, and refuses to do that without
//! `-f` when nobody is at the terminal to confirm.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use gitcko_config::GitckoPaths;
use gitcko_git::Repo;
use gitcko_index::{IndexLimits, Store, StoreError, build, build_incremental};
use tokio_util::sync::CancellationToken;

use crate::Io;
use crate::cli::{CacheArgs, CacheCmd, IndexArgs, PurgeArgs};
use crate::review::repo_cache_id;

/// Build or refresh the index for HEAD. `--full` does not reuse a previous parse.
pub fn index<O, E>(
    args: &IndexArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    io: &Io<O, E>,
) -> u8
where
    O: Write + Send,
    E: Write + Send,
{
    match index_inner(args, cwd, env, io) {
        Ok(code) => code,
        Err((code, message)) => {
            let _ = writeln!(lock(&io.stderr), "{message}");
            code
        }
    }
}

/// `cache stats` or `cache purge`.
pub fn cache<O, E>(
    args: &CacheArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    stdin_is_tty: bool,
    io: &Io<O, E>,
) -> u8
where
    O: Write + Send,
    E: Write + Send,
{
    match &args.command {
        CacheCmd::Stats => stats(env, io),
        CacheCmd::Purge(purge) => purge_cache(purge, cwd, env, stdin_is_tty, io),
    }
}

fn index_inner<O, E>(
    args: &IndexArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    io: &Io<O, E>,
) -> Result<u8, (u8, String)>
where
    O: Write + Send,
    E: Write + Send,
{
    let repo = Repo::open(cwd).map_err(|_| (2, "not a git repository".to_string()))?;
    let head = repo
        .resolve("HEAD")
        .map_err(|_| (2, "could not resolve HEAD".to_string()))?;
    let paths = paths(env)?;
    let repo_id = repo_cache_id(cwd).map_err(|err| (err.code, err.message))?;
    let store = Store::new(&paths.cache_dir);
    let limits = IndexLimits::default();
    let revision = if args.full {
        build(&repo, head, &limits).map_err(|_| (1, "could not build the index".to_string()))?
    } else {
        match store.active(&repo_id) {
            Ok(Some(id)) => match store.open(&repo, &repo_id, &id) {
                Ok(previous) => build_incremental(&repo, head, &limits, &previous)
                    .map_err(|_| (1, "could not build the index".to_string()))?,
                Err(_) => build(&repo, head, &limits)
                    .map_err(|_| (1, "could not build the index".to_string()))?,
            },
            _ => build(&repo, head, &limits)
                .map_err(|_| (1, "could not build the index".to_string()))?,
        }
    };
    let cancel = CancellationToken::new();
    let _ctrl_c = if index_cancel_requested(env) {
        cancel.cancel();
        None
    } else {
        Some(spawn_index_ctrl_c_listener(cancel.clone()))
    };
    let mut keep_going = |step: gitcko_index::PublishStep| {
        let _ = writeln!(lock(&io.stderr), "stage: {}", step_name(step));
        !cancel.is_cancelled()
    };
    match store.publish(&repo_id, &revision, &mut keep_going) {
        Ok(id) => {
            let _ = writeln!(lock(&io.stdout), "indexed {id}");
            Ok(0)
        }
        Err(StoreError::Cancelled) => {
            let _ = writeln!(lock(&io.stderr), "status: canceled");
            Ok(130)
        }
        Err(_) => Err((1, "could not store the index".to_string())),
    }
}

fn step_name(step: gitcko_index::PublishStep) -> &'static str {
    match step {
        gitcko_index::PublishStep::Write => "write",
        gitcko_index::PublishStep::Sync => "sync",
        gitcko_index::PublishStep::Rename => "rename",
        gitcko_index::PublishStep::Activate => "activate",
    }
}

/// Stops the Ctrl-C listener and joins its thread when dropped.
struct IndexCtrlCListener {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl IndexCtrlCListener {
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for IndexCtrlCListener {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn spawn_index_ctrl_c_listener(cancel: CancellationToken) -> IndexCtrlCListener {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    let thread = std::thread::spawn(move || {
        if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            runtime.block_on(async move {
                loop {
                    tokio::select! {
                        res = tokio::signal::ctrl_c() => {
                            if res.is_ok() {
                                cancel.cancel();
                            }
                            break;
                        }
                        () = tokio::time::sleep(Duration::from_millis(50)) => {
                            if stop_flag.load(Ordering::Relaxed) {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    IndexCtrlCListener {
        stop,
        thread: Some(thread),
    }
}

/// Debug builds only. Both variables must be set, so a stray `GITCKO_TEST_INDEX_CANCEL`
/// does not cancel a normal `cargo run`. Release builds never read them.
fn index_cancel_requested(env: &BTreeMap<String, String>) -> bool {
    #[cfg(debug_assertions)]
    {
        env.get("GITCKO_TEST_SEAM").map(String::as_str) == Some("1")
            && env.get("GITCKO_TEST_INDEX_CANCEL").map(String::as_str) == Some("1")
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = env;
        false
    }
}

fn stats<O, E>(env: &BTreeMap<String, String>, io: &Io<O, E>) -> u8
where
    O: Write,
    E: Write,
{
    match stats_inner(env, io) {
        Ok(code) => code,
        Err((code, message)) => {
            let _ = writeln!(lock(&io.stderr), "{message}");
            code
        }
    }
}

fn stats_inner<O, E>(env: &BTreeMap<String, String>, io: &Io<O, E>) -> Result<u8, (u8, String)>
where
    O: Write,
    E: Write,
{
    let paths = paths(env)?;
    let store = Store::new(&paths.cache_dir);
    let sizes = store
        .sizes()
        .map_err(|_| (1, "could not read the cache".to_string()))?;
    let mut total = 0u64;
    for (id, bytes) in &sizes {
        total += bytes;
        let _ = writeln!(lock(&io.stdout), "{id}  {bytes}");
    }
    let _ = writeln!(lock(&io.stdout), "total  {total}");
    Ok(0)
}

fn purge_cache<O, E>(
    args: &PurgeArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    stdin_is_tty: bool,
    io: &Io<O, E>,
) -> u8
where
    O: Write,
    E: Write,
{
    match purge_inner(args, cwd, env, stdin_is_tty) {
        Ok(()) => 0,
        Err((code, message)) => {
            let _ = writeln!(lock(&io.stderr), "{message}");
            code
        }
    }
}

fn purge_inner(
    args: &PurgeArgs,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    stdin_is_tty: bool,
) -> Result<(), (u8, String)> {
    if !purge_confirmed(args.force, stdin_is_tty) {
        return Err((2, "pass -f to delete the cache".to_string()));
    }
    let paths = paths(env)?;
    let store = Store::new(&paths.cache_dir);
    if args.all {
        store
            .purge_all()
            .map_err(|_| (1, "could not delete the cache".to_string()))?;
        return Ok(());
    }
    let _repo = Repo::open(cwd).map_err(|_| (2, "not a git repository".to_string()))?;
    let repo_id = repo_cache_id(cwd).map_err(|err| (err.code, err.message))?;
    store
        .purge_repo(&repo_id)
        .map_err(|_| (1, "could not delete the cache".to_string()))?;
    Ok(())
}

/// `-f` deletes. Without it, a pipe is exit 2: there is no one to confirm, and this
/// build does not prompt on a terminal either, so a purge never happens by surprise.
pub fn purge_confirmed(force: bool, stdin_is_tty: bool) -> bool {
    let _ = stdin_is_tty;
    force
}

fn paths(env: &BTreeMap<String, String>) -> Result<GitckoPaths, (u8, String)> {
    GitckoPaths::from_env(env).map_err(|err| (2, err.to_string()))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
mod tests {
    use super::purge_confirmed;

    #[test]
    fn force_confirms_a_delete_and_a_pipe_does_not() {
        assert!(purge_confirmed(true, false));
        assert!(!purge_confirmed(false, false));
        assert!(
            !purge_confirmed(false, true),
            "this build does not prompt, so a terminal still needs -f"
        );
    }
}
