// SPDX-License-Identifier: AGPL-3.0-or-later

//! Review history on disk: permissions, atomic writes, running markers, typed errors.

use std::fs;
use std::path::Path;

use gitcko_protocol::review::Review;
use gitcko_redact::canaries;
use gitcko_storage::{ReviewStore, RunState, RunningMarker, StoreError};
use gitcko_test_support::TempDir;

const SAMPLE: &str = include_str!("../../gitcko-protocol/tests/data/review-full.json");
const REPO: &str = "demo";

fn sample() -> Review {
    serde_json::from_str(SAMPLE).expect("protocol fixture")
}

fn store() -> (TempDir, ReviewStore) {
    let dir = TempDir::new("reviews");
    let store = ReviewStore::new(dir.path());
    (dir, store)
}

#[test]
fn put_then_get_round_trips_the_review() {
    let (_dir, store) = store();
    let review = sample();
    store.put(REPO, &review).unwrap();
    let got = store.get(REPO, &review.review_id).unwrap();
    assert_eq!(got, review);
}

#[cfg(unix)]
#[test]
fn folders_are_0700_and_files_0600() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, store) = store();
    store.put(REPO, &sample()).unwrap();
    fn walk(p: &Path) {
        for e in fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            let mode = e.metadata().unwrap().permissions().mode() & 0o777;
            if e.file_type().unwrap().is_dir() {
                assert_eq!(mode, 0o700, "{}", e.path().display());
                walk(&e.path());
            } else {
                assert_eq!(mode, 0o600, "{}", e.path().display());
            }
        }
    }
    walk(&store.root().join("repos"));
}

#[test]
fn put_is_atomic_no_partial_json_on_success() {
    let (_dir, store) = store();
    let review = sample();
    store.put(REPO, &review).unwrap();
    let dir = store.root().join("repos").join(REPO).join("reviews");
    let names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .all(|n| n.ends_with(".json") && !n.contains(".tmp")),
        "{names:?}"
    );
}

#[test]
fn list_is_newest_first_and_skips_tmp_and_running() {
    let (_dir, store) = store();
    let mut older = sample();
    older.review_id = "rv_01AAAAAAAAAAAAAA0000000000".into();
    let mut newer = sample();
    newer.review_id = "rv_01ZZZZZZZZZZZZZZ0000000000".into();
    store.put(REPO, &older).unwrap();
    store.put(REPO, &newer).unwrap();
    store
        .mark_running(
            REPO,
            &newer.review_id,
            &RunningMarker {
                pid: std::process::id(),
                started: "2026-01-01T00:00:00Z".into(),
            },
        )
        .unwrap();
    let ids: Vec<_> = store
        .list(REPO)
        .unwrap()
        .into_iter()
        .map(|s| s.review_id)
        .collect();
    assert_eq!(ids, [newer.review_id, older.review_id]);
}

#[test]
fn a_live_pid_is_running_and_clearing_it_returns_idle() {
    let (_dir, store) = store();
    let id = &sample().review_id;
    let marker = RunningMarker {
        pid: std::process::id(),
        started: "2026-01-01T00:00:00Z".into(),
    };
    store.mark_running(REPO, id, &marker).unwrap();
    assert_eq!(
        store.run_state(REPO, id).unwrap(),
        RunState::Running(marker)
    );
    store.clear_running(REPO, id).unwrap();
    assert_eq!(store.run_state(REPO, id).unwrap(), RunState::Idle);
}

#[test]
fn a_marker_whose_pid_is_gone_is_stale_not_running() {
    let (_dir, store) = store();
    let id = &sample().review_id;
    // pid 1<<22 is not a live process on the machines we ship to; if it were,
    // the test would still fail closed (Running) rather than pass wrongly.
    let marker = RunningMarker {
        pid: 4_194_304,
        started: "2026-01-01T00:00:00Z".into(),
    };
    store.mark_running(REPO, id, &marker).unwrap();
    assert_eq!(store.run_state(REPO, id).unwrap(), RunState::Stale(marker));
}

#[test]
fn a_live_pid_control_proves_the_stale_check_can_see_a_process() {
    let (_dir, store) = store();
    let id = &sample().review_id;
    let marker = RunningMarker {
        pid: std::process::id(),
        started: "2026-01-01T00:00:00Z".into(),
    };
    store.mark_running(REPO, id, &marker).unwrap();
    match store.run_state(REPO, id).unwrap() {
        RunState::Running(got) => assert_eq!(got.pid, marker.pid),
        other => panic!("live pid must be Running, got {other:?}"),
    }
}

#[test]
fn corrupt_json_is_a_typed_error_not_a_panic() {
    let (dir, store) = store();
    let review = sample();
    let path = dir
        .path()
        .join("repos")
        .join(REPO)
        .join("reviews")
        .join(format!("{}.json", review.review_id));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{not json").unwrap();
    assert_eq!(
        store.get(REPO, &review.review_id).unwrap_err(),
        StoreError::Corrupt { check: "json" }
    );
}

#[test]
fn canary_corrupt_contents_never_appear_in_the_error() {
    let canary = canaries()[1].clone();
    let (dir, store) = store();
    let review = sample();
    let path = dir
        .path()
        .join("repos")
        .join(REPO)
        .join("reviews")
        .join(format!("{}.json", review.review_id));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &canary).unwrap();
    let shown = fs::read_to_string(&path).unwrap();
    assert!(
        shown.contains(&canary),
        "control: the file holds the canary"
    );
    let err = store.get(REPO, &review.review_id).unwrap_err();
    assert!(!err.to_string().contains(&canary), "{err}");
}

#[test]
fn missing_review_is_not_found() {
    let (_dir, store) = store();
    assert_eq!(
        store.get(REPO, &sample().review_id).unwrap_err(),
        StoreError::NotFound
    );
}

#[test]
fn a_hostile_repo_id_is_rejected_before_any_path_is_joined() {
    let (_dir, store) = store();
    let review = sample();
    assert_eq!(
        store.put("..", &review).unwrap_err(),
        StoreError::InvalidRepoId
    );
    assert_eq!(
        store.put("CON", &review).unwrap_err(),
        StoreError::InvalidRepoId
    );
}

#[test]
fn review_id_in_the_file_must_match_the_name() {
    let (_dir, store) = store();
    let mut review = sample();
    store.put(REPO, &review).unwrap();
    review.review_id = "rv_01BBBBBBBBBBBBBB0000000000".into();
    // Overwrite the bytes so the filename still has the original id.
    let path = store
        .root()
        .join("repos")
        .join(REPO)
        .join("reviews")
        .join("rv_01J9ZQ3V7K2M4N6P8R0S2T4V6X.json");
    fs::write(&path, serde_json::to_vec(&review).unwrap()).unwrap();
    assert_eq!(
        store
            .get(REPO, "rv_01J9ZQ3V7K2M4N6P8R0S2T4V6X")
            .unwrap_err(),
        StoreError::Corrupt { check: "id" }
    );
}
