// SPDX-License-Identifier: AGPL-3.0-or-later

//! The on-disk store (INDEXING §24–26, §31): atomic publish, corruption detection,
//! stale-version purge, locks, permissions, eviction, and a hostile repository.

use std::path::Path;

use gitcko_git::Repo;
use gitcko_index::{
    CodeQuery, CodeSearchMode, IndexLimits, PublishStep, RetrievalBudget, Revision, Store,
    StoreError, build,
};
use gitcko_test_support::{TempDir, empty_repo, fixture_repo, literal_commit};

const V1: &[(&[u8], &str, &[u8])] = &[
    (b"Cargo.toml", "100644", b"[package]\nname = \"demo\"\n"),
    (
        b"src/lib.rs",
        "100644",
        b"mod util;\n\npub fn total(xs: &[u32]) -> u32 {\n    util::helper(xs)\n}\n",
    ),
    (
        b"src/util.rs",
        "100644",
        b"pub fn helper(xs: &[u32]) -> u32 {\n    xs.len() as u32\n}\n",
    ),
];

/// A repo with two commits, and the revisions built for each.
fn two_revisions() -> (Repo, TempDir, Revision, Revision) {
    let dir = empty_repo("store");
    let first = literal_commit(dir.path(), V1);
    let mut v2: Vec<(&[u8], &str, &[u8])> = V1.to_vec();
    v2.push((b"src/extra.rs", "100644", b"pub fn extra() {}\n"));
    let second = literal_commit(dir.path(), &v2);
    let repo = Repo::open(dir.path()).unwrap();
    let limits = IndexLimits::default();
    let a = build(&repo, repo.resolve(&first).unwrap(), &limits).unwrap();
    let b = build(&repo, repo.resolve(&second).unwrap(), &limits).unwrap();
    (repo, dir, a, b)
}

fn go(_: PublishStep) -> bool {
    true
}

fn assert_same(a: &Revision, b: &Revision) {
    assert_eq!(a.commit, b.commit);
    assert_eq!(a.manifest, b.manifest);
    assert_eq!(a.parsed, b.parsed);
    assert_eq!(a.graph, b.graph);
    assert_eq!(a.chunks, b.chunks);
    assert_eq!(a.coverage, b.coverage);
    assert_eq!(a.versions, b.versions);
    assert_eq!(a.lexical.digests().unwrap(), b.lexical.digests().unwrap());
}

fn entries(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn index_dir(root: &Path, repo_id: &str) -> std::path::PathBuf {
    let base = root.join("repos").join(repo_id).join("index");
    let versions = entries(&base);
    assert_eq!(versions.len(), 1, "{versions:?}");
    base.join(&versions[0])
}

#[test]
fn a_published_revision_reopens_identical() {
    let (repo, _dir, a, _) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let id = store.publish("demo", &a, &mut go).unwrap();
    assert_eq!(store.active("demo").unwrap().as_deref(), Some(id.as_str()));
    let opened = store.open(&repo, "demo", &id).unwrap();
    assert_same(&opened, &a);
    // Publishing the same commit again is a no-op with the same id.
    assert_eq!(store.publish("demo", &a, &mut go).unwrap(), id);
    // And it answers queries.
    let hits = opened
        .search_code(
            &repo,
            &CodeQuery {
                text: "helper".into(),
                mode: CodeSearchMode::Lexical(gitcko_search::QueryMode::Identifier),
                language: None,
                role: None,
                path_prefix: None,
                limit: 10,
                offset: 0,
            },
            0,
        )
        .unwrap();
    assert!(!hits.hits.is_empty());
}

#[test]
fn stopping_at_any_step_never_moves_active() {
    let (repo, _dir, a, b) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let first = store.publish("demo", &a, &mut go).unwrap();
    for stop in PublishStep::ALL {
        let result = store.publish("demo", &b, &mut |step| step != stop);
        assert_eq!(result, Err(StoreError::Cancelled), "{stop:?}");
        assert_eq!(
            store.active("demo").unwrap().as_deref(),
            Some(first.as_str())
        );
        let staging = index_dir(root.path(), "demo").join("staging");
        assert!(
            entries(&staging).is_empty(),
            "{stop:?}: staging left behind"
        );
    }
    let second = store.publish("demo", &b, &mut go).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        store.active("demo").unwrap().as_deref(),
        Some(second.as_str())
    );
    // The older revision stays readable by id after `active` moves on.
    assert_same(&store.open(&repo, "demo", &first).unwrap(), &a);
}

#[test]
fn leftover_staging_is_swept_by_the_next_publish() {
    let (_repo, _dir, a, b) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    store.publish("demo", &a, &mut go).unwrap();
    // A crash mid-build leaves a half-written staging folder.
    let staging = index_dir(root.path(), "demo").join("staging");
    std::fs::create_dir_all(staging.join("crashed")).unwrap();
    std::fs::write(staging.join("crashed").join("chunks.json"), "[").unwrap();
    store.publish("demo", &b, &mut go).unwrap();
    assert!(entries(&staging).is_empty());
}

#[test]
fn a_corrupted_file_is_detected_not_loaded() {
    let (repo, _dir, a, _) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let id = store.publish("demo", &a, &mut go).unwrap();
    let file = index_dir(root.path(), "demo")
        .join("revisions")
        .join(&id)
        .join("chunks.json");
    let mut bytes = std::fs::read(&file).unwrap();
    let at = bytes.len() / 2;
    bytes[at] ^= 0x01;
    std::fs::write(&file, bytes).unwrap();
    assert!(matches!(
        store.open(&repo, "demo", &id),
        Err(StoreError::Corrupt { .. })
    ));
    // A corrupt revision is removed, so it's rebuilt rather than found again.
    assert!(!file.parent().unwrap().exists());
    store.publish("demo", &a, &mut go).unwrap();
    assert_same(&store.open(&repo, "demo", &id).unwrap(), &a);
    // A missing file is corruption too.
    std::fs::remove_file(&file).unwrap();
    assert!(matches!(
        store.open(&repo, "demo", &id),
        Err(StoreError::Corrupt { .. })
    ));
    // And a file the header lists outside the revision is never followed.
    store.publish("demo", &a, &mut go).unwrap();
    let rdir = file.parent().unwrap();
    let active = std::fs::read(rdir.join("../../active")).unwrap();
    let hash = gitcko_domain::ContentHash::of(&active).to_string();
    tamper(rdir, "revision.json", &mut |h| {
        h["files"]["../../active"] = hash.clone().into()
    });
    assert!(matches!(
        store.open(&repo, "demo", &id),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn an_old_schema_folder_is_never_read_and_is_purged() {
    let (_repo, _dir, a, _) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    store.publish("demo", &a, &mut go).unwrap();
    let current = index_dir(root.path(), "demo");
    let old = current.parent().unwrap().join("idx0.0.0.0-old");
    std::fs::create_dir_all(old.join("revisions").join("x")).unwrap();
    std::fs::write(old.join("active"), "x").unwrap();
    let removed = store.purge_stale("demo").unwrap();
    assert!(removed >= 1);
    assert!(!old.exists());
    assert!(current.exists());
    assert!(store.active("demo").unwrap().is_some());
}

#[test]
fn purge_never_removes_a_revision_being_read() {
    let (repo, _dir, a, b) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let first = store.publish("demo", &a, &mut go).unwrap();
    let reading = store.open(&repo, "demo", &first).unwrap();
    store.publish("demo", &b, &mut go).unwrap();
    store.purge_stale("demo").unwrap();
    let revisions = index_dir(root.path(), "demo").join("revisions");
    assert!(entries(&revisions).contains(&first), "removed while open");
    drop(reading);
    store.purge_stale("demo").unwrap();
    assert!(!entries(&revisions).contains(&first));
    assert_eq!(entries(&revisions).len(), 1, "the active one stays");
}

/// A process forked while a revision is open inherits its lock until it execs, so the
/// lock can outlive the reader for an instant; purge waits that out.
#[test]
fn purge_waits_out_a_lock_held_for_an_instant() {
    let (repo, _dir, a, b) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let first = store.publish("demo", &a, &mut go).unwrap();
    drop(store.open(&repo, "demo", &first).unwrap());
    store.publish("demo", &b, &mut go).unwrap();
    let revisions = index_dir(root.path(), "demo").join("revisions");
    let lock = std::fs::File::open(revisions.join(&first).join("lock")).unwrap();
    lock.lock_shared().unwrap();
    let held = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(lock);
    });
    store.purge_stale("demo").unwrap();
    held.join().unwrap();
    assert!(!entries(&revisions).contains(&first));
}

#[cfg(unix)]
#[test]
fn folders_are_0700_and_files_0600() {
    use std::os::unix::fs::PermissionsExt;
    let (_repo, _dir, a, _) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    store.publish("demo", &a, &mut go).unwrap();
    fn walk(p: &Path) {
        for e in std::fs::read_dir(p).unwrap() {
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
    walk(&root.path().join("repos"));
    walk(&root.path().join("locks"));
}

#[test]
fn eviction_drops_the_least_recently_used_repo_first() {
    let (repo, _dir, a, b) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    store.publish("old", &a, &mut go).unwrap();
    store.publish("kept", &b, &mut go).unwrap();
    let sizes = store.sizes().unwrap();
    assert_eq!(sizes.len(), 2);
    assert!(sizes.iter().all(|(_, bytes)| *bytes > 0));
    // A cap of one byte can't be met, but the repo in use is never evicted.
    let evicted = store.evict_to(1, "kept").unwrap();
    assert_eq!(evicted, ["old"]);
    assert!(store.active("old").unwrap().is_none());
    let id = store.active("kept").unwrap().unwrap();
    store.open(&repo, "kept", &id).unwrap();
}

#[test]
fn purge_all_and_one_repo() {
    let (_repo, _dir, a, _) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    store.publish("one", &a, &mut go).unwrap();
    store.publish("two", &a, &mut go).unwrap();
    store.purge_repo("one").unwrap();
    assert!(store.active("one").unwrap().is_none());
    assert!(store.active("two").unwrap().is_some());
    store.purge_all().unwrap();
    assert!(store.sizes().unwrap().is_empty());
}

#[test]
fn a_repo_id_can_never_name_a_path() {
    let (_repo, _dir, a, _) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    for bad in [
        "",
        "..",
        "../x",
        "a/b",
        "a\\b",
        "C:",
        "UPPER",
        &"x".repeat(65),
    ] {
        assert_eq!(
            store.publish(bad, &a, &mut go),
            Err(StoreError::InvalidRepoId),
            "{bad:?}"
        );
    }
    assert!(!root.path().join("x").exists());
}

#[test]
fn canary_store_errors_never_echo_values() {
    let (repo, _dir, a, _) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    for canary in gitcko_redact::canaries() {
        let errors = [
            store
                .publish(&format!("../{canary}"), &a, &mut go)
                .unwrap_err(),
            store.open(&repo, "demo", &canary).err().unwrap(),
        ];
        for err in errors {
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains(&canary), "{shown}");
            assert!(
                !shown.contains(&root.path().display().to_string()),
                "{shown}"
            );
        }
    }
}

#[test]
fn a_hostile_repository_is_indexed_and_queried_without_running_anything() {
    let dir = fixture_repo("hostile_index");
    let pwned = dir.path().parent().unwrap().join("PWNED");
    let repo = Repo::open(dir.path()).unwrap();
    let head = repo.resolve("HEAD").unwrap();
    let started = std::time::Instant::now();
    let rev = build(&repo, head, &IndexLimits::default()).unwrap();
    // The limits held: the pathological files are partial, not fatal.
    assert!(!rev.coverage.parsing.is_complete() || !rev.coverage.lexical.is_complete());
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let id = store.publish("hostile", &rev, &mut go).unwrap();
    let opened = store.open(&repo, "hostile", &id).unwrap();
    let query = CodeQuery {
        text: "PWNED".into(),
        mode: CodeSearchMode::Literal,
        language: None,
        role: None,
        path_prefix: None,
        limit: 50,
        offset: 0,
    };
    assert!(
        !opened
            .search_code(&repo, &query, 1 << 30)
            .unwrap()
            .hits
            .is_empty()
    );
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -3,1 +3,1 @@\n-    2\n+    1\n";
    opened.retrieve(
        &gitcko_diff::parse(diff).unwrap(),
        &RetrievalBudget::default(),
    );
    assert!(started.elapsed().as_secs() < 60, "{:?}", started.elapsed());
    assert!(!pwned.exists(), "something ran");
}

/// Rewrites one stored JSON file through `change` and records its new checksum, so the
/// files stay consistent and only deeper checks can catch the change.
fn tamper(rdir: &Path, name: &str, change: &mut dyn FnMut(&mut serde_json::Value)) {
    let path = rdir.join(name);
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    change(&mut value);
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    if name != "revision.json" {
        let hash = gitcko_domain::ContentHash::of(&bytes).to_string();
        tamper(rdir, "revision.json", &mut |h| {
            h["files"][name] = hash.clone().into()
        });
    }
}

#[test]
fn a_consistent_but_wrong_revision_fails_its_checks() {
    let (repo, _dir, a, _) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let id = store.publish("demo", &a, &mut go).unwrap();
    let rdir = index_dir(root.path(), "demo").join("revisions").join(&id);
    let corrupt = |store: &Store| {
        matches!(
            store.open(&repo, "demo", &id),
            Err(StoreError::Corrupt { .. })
        )
    };
    // Chunk text that no longer matches the commit: only validation sees it.
    tamper(&rdir, "chunks.json", &mut |c| {
        c[0]["text"] = "changed".into()
    });
    assert!(corrupt(&store));
    store.publish("demo", &a, &mut go).unwrap();
    // A parse result pointing past the manifest.
    tamper(&rdir, "parsed.json", &mut |p| p[0]["entry"] = 9999.into());
    assert!(corrupt(&store));
    store.publish("demo", &a, &mut go).unwrap();
    // A header that stops vouching for one of the files.
    tamper(&rdir, "revision.json", &mut |h| {
        h["files"].as_object_mut().unwrap().remove("coverage.json");
    });
    assert!(corrupt(&store));
    store.publish("demo", &a, &mut go).unwrap();
    // An intact revision under another revision's id.
    let other = index_dir(root.path(), "demo")
        .join("revisions")
        .join("0".repeat(64));
    std::fs::rename(&rdir, &other).unwrap();
    assert!(matches!(
        store.open(&repo, "demo", &"0".repeat(64)),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn eviction_goes_oldest_first_and_stops_at_the_cap() {
    let (_repo, _dir, a, b) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    // Published oldest first; alphabetical order would pick `mid` first.
    store.publish("old", &a, &mut go).unwrap();
    store.publish("mid", &a, &mut go).unwrap();
    store.publish("kept", &b, &mut go).unwrap();
    let total: u64 = store.sizes().unwrap().iter().map(|(_, s)| s).sum();
    assert_eq!(store.evict_to(total - 1, "kept").unwrap(), ["old"]);
    assert!(store.active("mid").unwrap().is_some());
}

#[test]
fn malformed_parse_results_are_corrupt_not_a_crash() {
    let (repo, _dir, a, _) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let id = store.publish("demo", &a, &mut go).unwrap();
    let rdir = index_dir(root.path(), "demo").join("revisions").join(&id);
    let lib = |p: &serde_json::Value| {
        p.as_array()
            .unwrap()
            .iter()
            .position(|f| !f["extraction"]["symbols"].as_array().unwrap().is_empty())
            .unwrap()
    };
    // A scope index past the scopes.
    tamper(&rdir, "parsed.json", &mut |p| {
        let i = lib(p);
        p[i]["extraction"]["symbols"][0]["scope"] = 999.into();
    });
    assert!(matches!(
        store.open(&repo, "demo", &id),
        Err(StoreError::Corrupt { .. })
    ));
    store.publish("demo", &a, &mut go).unwrap();
    // A scope that is its own parent.
    tamper(&rdir, "parsed.json", &mut |p| {
        let i = lib(p);
        p[i]["extraction"]["scopes"] = serde_json::json!([{"parent": 0, "name": "x"}]);
        p[i]["extraction"]["symbols"][0]["scope"] = 0.into();
    });
    assert!(matches!(
        store.open(&repo, "demo", &id),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn a_revision_relabelled_with_another_commit_is_corrupt() {
    let (repo, _dir, a, b) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let id_a = store.publish("demo", &a, &mut go).unwrap();
    let id_b = store.publish("demo", &b, &mut go).unwrap();
    let revisions = index_dir(root.path(), "demo").join("revisions");
    std::fs::remove_dir_all(revisions.join(&id_a)).unwrap();
    std::fs::rename(revisions.join(&id_b), revisions.join(&id_a)).unwrap();
    let commit = a.commit.clone();
    tamper(&revisions.join(&id_a), "revision.json", &mut |h| {
        h["commit"] = commit.clone().into()
    });
    assert!(matches!(
        store.open(&repo, "demo", &id_a),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn every_search_file_must_be_vouched_for() {
    let (repo, _dir, a, _) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let id = store.publish("demo", &a, &mut go).unwrap();
    let rdir = index_dir(root.path(), "demo").join("revisions").join(&id);
    tamper(&rdir, "revision.json", &mut |h| {
        let files = h["files"].as_object_mut().unwrap();
        let lexical = files
            .keys()
            .find(|k| k.starts_with("lexical/"))
            .unwrap()
            .clone();
        files.remove(&lexical);
    });
    assert!(matches!(
        store.open(&repo, "demo", &id),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn purge_waits_for_a_publish_in_progress() {
    let (repo, _dir, a, b) = two_revisions();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    store.publish("demo", &a, &mut go).unwrap();
    // Between the rename and the `active` swap, the new revision isn't active yet: a
    // purge started then waits for the publish to finish.
    std::thread::scope(|scope| {
        let mut purge = None;
        let id = store
            .publish("demo", &b, &mut |step| {
                if step == PublishStep::Activate {
                    let handle = scope.spawn(|| store.purge_stale("demo"));
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    assert!(!handle.is_finished(), "purge didn't wait");
                    purge = Some(handle);
                }
                true
            })
            .unwrap();
        assert!(purge.unwrap().join().unwrap().is_ok());
        assert_eq!(store.active("demo").unwrap().as_deref(), Some(id.as_str()));
        assert_same(&store.open(&repo, "demo", &id).unwrap(), &b);

        // Removing the whole repository waits too, then removes what was published.
        let mut purge = None;
        store
            .publish("demo", &a, &mut |step| {
                if step == PublishStep::Activate {
                    let handle = scope.spawn(|| store.purge_repo("demo"));
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    assert!(!handle.is_finished(), "purge didn't wait");
                    purge = Some(handle);
                }
                true
            })
            .unwrap();
        assert!(purge.unwrap().join().unwrap().is_ok());
        assert_eq!(store.active("demo").unwrap(), None);
    });
}

#[test]
fn a_revision_of_same_shaped_commit_is_not_this_one() {
    // Same paths and kinds, different content: only the object ids tell them apart.
    let dir = empty_repo("store-shape");
    let first = literal_commit(dir.path(), V1);
    let mut edited: Vec<(&[u8], &str, &[u8])> = V1.to_vec();
    edited[2].2 = b"pub fn helper(xs: &[u32]) -> u32 {\n    0\n}\n";
    let second = literal_commit(dir.path(), &edited);
    let repo = Repo::open(dir.path()).unwrap();
    let limits = IndexLimits::default();
    let a = build(&repo, repo.resolve(&first).unwrap(), &limits).unwrap();
    let c = build(&repo, repo.resolve(&second).unwrap(), &limits).unwrap();
    let root = TempDir::new("store-root");
    let store = Store::new(root.path());
    let id_a = store.publish("demo", &a, &mut go).unwrap();
    let id_c = store.publish("demo", &c, &mut go).unwrap();
    let revisions = index_dir(root.path(), "demo").join("revisions");
    std::fs::remove_dir_all(revisions.join(&id_a)).unwrap();
    std::fs::rename(revisions.join(&id_c), revisions.join(&id_a)).unwrap();
    let commit = a.commit.clone();
    tamper(&revisions.join(&id_a), "revision.json", &mut |h| {
        h["commit"] = commit.clone().into()
    });
    assert!(matches!(
        store.open(&repo, "demo", &id_a),
        Err(StoreError::Corrupt { .. })
    ));
}
