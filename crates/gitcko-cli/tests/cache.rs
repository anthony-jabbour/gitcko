// SPDX-License-Identifier: AGPL-3.0-or-later

//! `index` and `cache` against a temp cache. No network.

use std::collections::BTreeMap;

use gitcko_cli::{Io, RunEnv, execute_at, repo_cache_id};
use gitcko_storage::ReviewStore;
use gitcko_test_support::{TempDir, empty_repo, git};

struct World {
    repo: TempDir,
    _home: TempDir,
    env: BTreeMap<String, String>,
    cache: std::path::PathBuf,
}

fn world() -> World {
    let repo = empty_repo("cache");
    std::fs::write(repo.path().join("README"), b"hello\n").unwrap();
    assert!(
        git(repo.path())
            .args(["add", "README"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        git(repo.path())
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@fixture.invalid",
                "commit",
                "-q",
                "-m",
                "init",
            ])
            .status()
            .unwrap()
            .success()
    );
    let home = TempDir::new("cache-home");
    let home_path = home.path().canonicalize().unwrap();
    let cache = home_path.join("cache");
    std::fs::create_dir(&cache).unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), home_path.display().to_string()),
        ("APPDATA".into(), home_path.display().to_string()),
        (
            "LOCALAPPDATA".into(),
            home_path.join("local").display().to_string(),
        ),
        (
            "GITCKO_CONFIG".into(),
            home_path.join("config.toml").display().to_string(),
        ),
        ("GITCKO_CACHE_DIR".into(), cache.display().to_string()),
    ]);
    World {
        repo,
        _home: home,
        env,
        cache,
    }
}

fn go(world: &World, args: &[&str]) -> (u8, String, String) {
    let io = Io::capture();
    let code = execute_at(
        args,
        &RunEnv {
            cwd: world.repo.path(),
            env: &world.env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
        },
        &io,
    );
    (code, io.stdout_text(), io.stderr_text())
}

fn plant_review(world: &World) {
    let review = serde_json::from_str::<gitcko_protocol::review::Review>(include_str!(
        "../../gitcko-protocol/tests/data/review-full.json"
    ))
    .unwrap();
    let store = ReviewStore::new(&world.cache);
    store
        .put(&repo_cache_id(world.repo.path()).unwrap(), &review)
        .unwrap();
}

#[test]
fn stats_grows_when_a_review_is_stored_and_starts_at_zero() {
    let world = world();
    let (code, out, err) = go(&world, &["cache", "stats"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "total  0\n");

    plant_review(&world);
    let (code, out, err) = go(&world, &["cache", "stats"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains(&repo_cache_id(world.repo.path()).unwrap()),
        "{out}"
    );
    assert!(!out.ends_with("total  0\n"), "{out}");
}

#[test]
fn purge_without_force_keeps_the_cache_and_force_deletes_one_repo() {
    let world = world();
    plant_review(&world);
    std::fs::create_dir_all(world.cache.join("repos").join("otherrepo")).unwrap();
    std::fs::write(
        world.cache.join("repos").join("otherrepo").join("note"),
        b"keep",
    )
    .unwrap();

    let (code, _, err) = go(&world, &["cache", "purge", "--repo"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("-f"), "{err}");
    let (code, out, _) = go(&world, &["cache", "stats"]);
    assert_eq!(code, 0);
    assert!(out.contains("otherrepo"), "{out}");
    assert!(
        out.contains(&repo_cache_id(world.repo.path()).unwrap()),
        "{out}"
    );

    let (code, _, err) = go(&world, &["cache", "purge", "--repo", "-f"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, _) = go(&world, &["cache", "stats"]);
    assert_eq!(code, 0);
    assert!(
        !out.contains(&repo_cache_id(world.repo.path()).unwrap()),
        "{out}"
    );
    assert!(
        out.contains("otherrepo"),
        "the other repository stays: {out}"
    );

    let (code, _, err) = go(&world, &["cache", "purge", "--all", "-f"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, _) = go(&world, &["cache", "stats"]);
    assert_eq!(code, 0);
    assert_eq!(out, "total  0\n");
}

#[test]
fn index_publishes_a_revision() {
    let world = world();
    let (code, out, err) = go(&world, &["index"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("indexed "), "{out}");
    assert!(err.contains("stage: "), "{err}");

    let (again, out2, err) = go(&world, &["index"]);
    assert_eq!(again, 0, "{err}");
    assert!(out2.starts_with("indexed "), "{out2}");
}

#[cfg(debug_assertions)]
#[test]
fn index_cancel_via_debug_seam_is_130() {
    let world = world();
    let mut env = world.env.clone();
    env.insert("GITCKO_TEST_SEAM".into(), "1".into());
    env.insert("GITCKO_TEST_INDEX_CANCEL".into(), "1".into());
    let io = Io::capture();
    let code = execute_at(
        &["index", "--full"],
        &RunEnv {
            cwd: world.repo.path(),
            env: &env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
        },
        &io,
    );
    assert_eq!(code, 130, "{}", io.stderr_text());
    assert!(io.stderr_text().contains("status: canceled"));
}

#[test]
fn index_needs_a_repository() {
    let world = world();
    let bare = TempDir::new("not-a-repo");
    let io = Io::capture();
    let code = execute_at(
        &["index"],
        &RunEnv {
            cwd: bare.path(),
            env: &world.env,
            runner: None,
            stdout_is_tty: false,
            stdin_is_tty: false,
        },
        &io,
    );
    assert_eq!(code, 2, "{}", io.stderr_text());
}
