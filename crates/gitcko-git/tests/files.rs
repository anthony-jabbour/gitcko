// SPDX-License-Identifier: AGPL-3.0-or-later

//! Listing every entry of a commit's tree, for index discovery.

use gitcko_git::{EntryKind, Repo};
use gitcko_test_support::{empty_repo, literal_commit};

const SUBMODULE: &[u8] = b"1111111111111111111111111111111111111111";

#[test]
fn files_lists_every_non_folder_entry_sorted_by_path_bytes() {
    let dir = empty_repo("files");
    let commit = literal_commit(
        dir.path(),
        &[
            (b"src/main.rs", "100644", b"fn main() {}\n"),
            (b"run.sh", "100755", b"#!/bin/sh\n"),
            (b"link", "120000", b"/etc/passwd"),
            (b"vendor/lib", "160000", SUBMODULE),
            (b"README.md", "100644", b"hi\n"),
            // Breadth-first order would list `b.rs` before `a/z.rs`; sorted is the reverse.
            (b"a/z.rs", "100644", b"z"),
            (b"b.rs", "100644", b"b"),
        ],
    );
    let repo = Repo::open(dir.path()).unwrap();
    let files = repo.files(repo.resolve(&commit).unwrap()).unwrap();
    let listed: Vec<(&[u8], EntryKind)> =
        files.iter().map(|f| (f.path.as_slice(), f.kind)).collect();
    assert_eq!(
        listed,
        [
            (&b"README.md"[..], EntryKind::File),
            (b"a/z.rs", EntryKind::File),
            (b"b.rs", EntryKind::File),
            (b"link", EntryKind::Symlink),
            (b"run.sh", EntryKind::Executable),
            (b"src/main.rs", EntryKind::File),
            (b"vendor/lib", EntryKind::Submodule),
        ]
    );
}

#[test]
fn files_keeps_names_no_filesystem_allows_instead_of_failing() {
    // Discovery must give each of these a disposition, so listing can't stop at them.
    let dir = empty_repo("files-hostile");
    let names: [&[u8]; 6] = [
        b"CON",
        b"a/../b",
        b".git/config",
        b"Readme",
        b"README",
        b"bad\xff.rs",
    ];
    let files: Vec<_> = names.iter().map(|n| (*n, "100644", &b"x"[..])).collect();
    let commit = literal_commit(dir.path(), &files);
    let repo = Repo::open(dir.path()).unwrap();
    let mut listed: Vec<Vec<u8>> = repo
        .files(repo.resolve(&commit).unwrap())
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    listed.sort();
    let mut expected: Vec<Vec<u8>> = names.iter().map(|n| n.to_vec()).collect();
    expected.sort();
    assert_eq!(listed, expected);
}

#[test]
fn blob_size_is_known_before_the_blob_is_read() {
    let dir = empty_repo("files-size");
    let big = vec![b'x'; 300_000];
    let commit = literal_commit(dir.path(), &[(b"big.txt", "100644", &big)]);
    let repo = Repo::open(dir.path()).unwrap();
    let file = &repo.files(repo.resolve(&commit).unwrap()).unwrap()[0];
    assert_eq!(repo.blob_size(file.id).unwrap(), 300_000);
    assert_eq!(repo.read_blob_id(file.id).unwrap(), big);
}

#[test]
fn a_submodule_id_is_never_read_as_a_blob() {
    // The commit a gitlink names lives in another repository; asking for it is an
    // error naming the object id only.
    let dir = empty_repo("files-submodule");
    let commit = literal_commit(dir.path(), &[(b"vendor/lib", "160000", SUBMODULE)]);
    let repo = Repo::open(dir.path()).unwrap();
    let file = &repo.files(repo.resolve(&commit).unwrap()).unwrap()[0];
    let err = repo.blob_size(file.id).unwrap_err().to_string();
    assert!(err.contains(&"1".repeat(40)), "{err}");
    assert!(repo.read_blob_id(file.id).is_err());
}

#[test]
fn a_submodule_pointing_at_a_commit_in_this_repo_is_still_not_a_blob() {
    // Only the object's kind tells a reachable commit from a blob.
    let dir = empty_repo("files-own-commit");
    let first = literal_commit(dir.path(), &[(b"a.txt", "100644", b"a")]);
    let commit = literal_commit(dir.path(), &[(b"self", "160000", first.as_bytes())]);
    let repo = Repo::open(dir.path()).unwrap();
    let file = &repo.files(repo.resolve(&commit).unwrap()).unwrap()[0];
    assert_eq!(file.kind, EntryKind::Submodule);
    assert!(repo.blob_size(file.id).is_err());
}
