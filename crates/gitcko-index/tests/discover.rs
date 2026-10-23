// SPDX-License-Identifier: AGPL-3.0-or-later

//! Discovery: every path of a commit gets one disposition (INDEXING §7–9, §38.1).

use gitcko_domain::{Coverage, RepoPath};
use gitcko_git::{GitError, Repo};
use gitcko_index::{IndexError, IndexLimits, Manifest, discover};
use gitcko_test_support::{empty_repo, literal_commit};

const SUBMODULE: &[u8] = b"1111111111111111111111111111111111111111";

fn manifest(files: &[(&[u8], &str, &[u8])], limits: &IndexLimits) -> Manifest {
    let dir = empty_repo("discover");
    let commit = literal_commit(dir.path(), files);
    let repo = Repo::open(dir.path()).unwrap();
    discover(&repo, repo.resolve(&commit).unwrap(), limits).unwrap()
}

/// One line per path, with no local paths in it, so one snapshot fits every OS.
fn render(manifest: &Manifest) -> String {
    let mut out = format!("coverage: {:?}\n", manifest.coverage);
    for e in &manifest.entries {
        let mut flags = Vec::new();
        for (on, name) in [
            (e.binary, "binary"),
            (e.generated, "generated"),
            (e.vendor, "vendor"),
            (e.ignored_by_config, "ignored"),
            (e.case_collision, "case_collision"),
        ] {
            if on {
                flags.push(name);
            }
        }
        let hash = e.content_hash.map(|h| h.to_string()[..12].to_string());
        out += &format!(
            "{} | {:?} | size {:?} | hash {:?} | {:?} | {:?} | [{}] | {:?} {}\n",
            e.path,
            e.kind,
            e.size,
            hash,
            e.language,
            e.role,
            flags.join(","),
            e.disposition,
            e.reason.as_ref().map(|r| r.as_str()).unwrap_or("-"),
        );
    }
    out
}

fn small_limits() -> IndexLimits {
    IndexLimits {
        max_parse_bytes: 64,
        max_lexical_bytes: 256,
        ..IndexLimits::default()
    }
}

#[test]
fn every_kind_of_path_gets_one_disposition() {
    let big = vec![b'a'; 300];
    let mid = format!("// {}\n", "x".repeat(96));
    let lfs = b"version https://git-lfs.github.com/spec/v1\noid sha256:4d7a\nsize 99999\n";
    let m = manifest(
        &[
            (b"src/main.rs", "100644", b"fn main() {}\n"),
            ("src/my file \u{fc}.rs".as_bytes(), "100644", b"fn a() {}\n"),
            (b"src/crlf.rs", "100644", b"fn a() {}\r\nfn b() {}\r\n"),
            (b"src/gen.rs", "100644", b"// @generated\nfn g() {}\n"),
            (b"README.md", "100644", b"# hi\n"),
            (b"readme.md", "100644", b"other\n"),
            (b"Cargo.lock", "100644", b"# lock\n"),
            (b"vendor/dep/lib.rs", "100644", b"pub fn d() {}\n"),
            (b"fake.rs", "100644", b"\x7fELF\0\0\0"),
            (b"latin1.rs", "100644", b"fn \xe9() {}\n"),
            (b"assets/model.bin", "100644", lfs),
            (b"big.rs", "100644", &big),
            (b"mid.rs", "100644", mid.as_bytes()),
            (b"run.sh", "100755", b"echo hi\n"),
            (b"link-root", "120000", b"/"),
            (b"link-up", "120000", b"../.."),
            (b"third/sub", "160000", SUBMODULE),
            (b"CON", "100644", b"x"),
            (b"a/../escape.rs", "100644", b"x"),
            (b".git/config", "100644", b"x"),
            (b"bad\xff.rs", "100644", b"x"),
            (
                b".gitcko/config.toml",
                "100644",
                b"schema_version = 1\nignore = [\"vendor/\", \"*.lock\"]\n",
            ),
        ],
        &small_limits(),
    );
    assert!(!m.config_invalid);
    insta::assert_snapshot!(render(&m));
}

#[test]
fn the_file_limit_cuts_in_path_order_and_says_how_many_were_left_out() {
    let files: Vec<(&[u8], &str, &[u8])> = vec![
        (b"d.rs", "100644", b"d"),
        (b"a.rs", "100644", b"a"),
        (b"c.rs", "100644", b"c"),
        (b"b.rs", "100644", b"b"),
    ];
    let limits = IndexLimits {
        max_files: 2,
        ..IndexLimits::default()
    };
    let m = manifest(&files, &limits);
    let kept: Vec<_> = m
        .entries
        .iter()
        .map(|e| (e.path.as_str(), format!("{:?}", e.disposition)))
        .collect();
    assert_eq!(
        kept,
        [
            ("a.rs", "Parse".into()),
            ("b.rs", "Parse".into()),
            ("c.rs", "ExcludedResourceLimit".into()),
            ("d.rs", "ExcludedResourceLimit".into()),
        ]
    );
    let Coverage::Partial { reason, omitted } = &m.coverage else {
        panic!("{:?}", m.coverage);
    };
    assert_eq!((reason.as_str(), *omitted), ("repo_file_limit", Some(2)));
}

#[test]
fn the_byte_limit_stops_reading_once_the_repository_budget_is_spent() {
    let files: Vec<(&[u8], &str, &[u8])> = vec![
        (b"a.rs", "100644", &[b'a'; 40]),
        (b"b.rs", "100644", &[b'b'; 40]),
        (b"c.rs", "100644", &[b'c'; 10]),
    ];
    let limits = IndexLimits {
        max_repo_bytes: 60,
        ..IndexLimits::default()
    };
    let m = manifest(&files, &limits);
    assert!(m.entries[0].content_hash.is_some());
    // b.rs would go over the budget; c.rs would fit, but the cut is a prefix of the
    // sorted paths so it's the same whatever order files are added in.
    for e in &m.entries[1..] {
        assert_eq!(e.content_hash, None, "{}", e.path);
        assert_eq!(e.reason.as_ref().unwrap().as_str(), "repo_byte_limit");
    }
    assert!(matches!(
        m.coverage,
        Coverage::Partial {
            omitted: Some(2),
            ..
        }
    ));
}

#[test]
fn a_broken_root_config_falls_back_to_the_default_ignore_list() {
    let m = manifest(
        &[
            (b".gitcko/config.toml", "100644", b"not = [valid toml"),
            (b"Cargo.lock", "100644", b"# lock\n"),
        ],
        &IndexLimits::default(),
    );
    assert!(m.config_invalid);
    let lock = m.entries.iter().find(|e| e.path == "Cargo.lock").unwrap();
    assert!(lock.ignored_by_config, "the default list skips lockfiles");
    assert_eq!(m.coverage, Coverage::Complete {});
}

#[test]
fn canary_index_errors_never_show_a_repository_path() {
    for canary in gitcko_redact::canaries() {
        let Ok(path) = RepoPath::parse(&canary.replace(['\\', ':'], "_")) else {
            continue;
        };
        for err in [
            GitError::Link { path: path.clone() },
            GitError::Read {
                path: path.clone(),
                reason: canary.clone(),
            },
            GitError::NotRegularFile { path },
        ] {
            let err = IndexError::from(err);
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains(&canary), "{shown}");
        }
    }
}

#[test]
fn a_file_too_big_to_read_doesnt_spend_the_repository_budget() {
    // b.rs is skipped for its own size; it must not count against the budget or start
    // the cut, so c.rs still fits.
    let files: Vec<(&[u8], &str, &[u8])> = vec![
        (b"a.rs", "100644", &[b'a'; 40]),
        (b"b.rs", "100644", &[b'b'; 55]),
        (b"c.rs", "100644", &[b'c'; 10]),
    ];
    let limits = IndexLimits {
        max_repo_bytes: 60,
        max_lexical_bytes: 50,
        ..IndexLimits::default()
    };
    let m = manifest(&files, &limits);
    let reasons: Vec<_> = m
        .entries
        .iter()
        .map(|e| e.reason.as_ref().map(|r| r.as_str().to_string()))
        .collect();
    assert_eq!(reasons, [None, Some("file_too_large".into()), None]);
    assert_eq!(m.coverage, Coverage::Complete {});
}

#[test]
fn only_a_folder_named_exactly_gitcko_holds_config() {
    let m = manifest(
        &[
            (
                b"evil.gitcko/config.toml",
                "100644",
                b"schema_version = 1\nignore = [\"src/\"]\n",
            ),
            (b"src/a.rs", "100644", b"fn a() {}\n"),
        ],
        &IndexLimits::default(),
    );
    let a = m.entries.iter().find(|e| e.path == "src/a.rs").unwrap();
    assert!(!a.ignored_by_config);
}

#[test]
fn config_files_have_their_own_budget_and_overflowing_it_falls_back_to_defaults() {
    // A hostile repository could hold thousands of `.gitcko/` folders. Past the budget,
    // no config is used at all (half a cascade could loosen it), and the manifest says so.
    let root = b"schema_version = 1\nignore = [\"*.gen.rs\"]\n";
    let names: Vec<String> = (0..4)
        .map(|i| format!("p{i}/.gitcko/config.toml"))
        .collect();
    let mut files: Vec<(&[u8], &str, &[u8])> = vec![
        (b".gitcko/config.toml", "100644", root),
        (b"a.gen.rs", "100644", b"fn a() {}\n"),
        (b"Cargo.lock", "100644", b"# lock\n"),
    ];
    for name in &names {
        files.push((name.as_bytes(), "100644", b"strictness = 3\n"));
    }
    let limits = IndexLimits {
        max_config_files: 3,
        ..IndexLimits::default()
    };
    let m = manifest(&files, &limits);
    assert!(m.config_invalid);
    let flag = |p: &str| {
        m.entries
            .iter()
            .find(|e| e.path == p)
            .unwrap()
            .ignored_by_config
    };
    assert!(!flag("a.gen.rs"), "the root config wasn't applied");
    assert!(flag("Cargo.lock"), "the defaults were");

    // The same files under a big enough budget use the root config.
    let m = manifest(&files, &IndexLimits::default());
    assert!(!m.config_invalid);
    let flag = |p: &str| {
        m.entries
            .iter()
            .find(|e| e.path == p)
            .unwrap()
            .ignored_by_config
    };
    assert!(flag("a.gen.rs"));

    // Bytes count too.
    let limits = IndexLimits {
        max_config_bytes: 40,
        ..IndexLimits::default()
    };
    assert!(manifest(&files, &limits).config_invalid);
}

/// Deletes a blob's loose object, as disk corruption or a broken partial clone would.
fn delete_object(repo: &std::path::Path, commit: &str, path: &str) {
    let out = gitcko_test_support::git(repo)
        .args(["rev-parse", &format!("{commit}:{path}")])
        .output()
        .unwrap();
    let id = String::from_utf8(out.stdout).unwrap();
    let id = id.trim();
    std::fs::remove_file(repo.join(".git/objects").join(&id[..2]).join(&id[2..])).unwrap();
}

#[test]
fn an_unreadable_file_is_unavailable_and_discovery_goes_on() {
    let dir = empty_repo("discover-unreadable");
    let commit = literal_commit(
        dir.path(),
        &[
            (b"a.rs", "100644", b"fn a() {}\n"),
            (b"gone.rs", "100644", b"fn gone() {}\n"),
            (
                b".gitcko/config.toml",
                "100644",
                b"schema_version = 1\nignore = [\"a.rs\"]\n",
            ),
        ],
    );
    delete_object(dir.path(), &commit, "gone.rs");
    delete_object(dir.path(), &commit, ".gitcko/config.toml");
    let repo = Repo::open(dir.path()).unwrap();
    let m = discover(
        &repo,
        repo.resolve(&commit).unwrap(),
        &IndexLimits::default(),
    )
    .unwrap();
    let gone = m.entries.iter().find(|e| e.path == "gone.rs").unwrap();
    assert_eq!(format!("{:?}", gone.disposition), "Unavailable");
    assert_eq!(gone.reason.as_ref().unwrap().as_str(), "unreadable");
    let a = m.entries.iter().find(|e| e.path == "a.rs").unwrap();
    assert_eq!(format!("{:?}", a.disposition), "Parse");
    // The unreadable config can't be applied, so the defaults are, and that's reported.
    assert!(m.config_invalid);
    assert!(!a.ignored_by_config);
    let Coverage::Partial { reason, omitted } = &m.coverage else {
        panic!("{:?}", m.coverage);
    };
    // gone.rs and the config file itself, which is a manifest entry too.
    assert_eq!((reason.as_str(), *omitted), ("unreadable_files", Some(2)));
}

#[test]
fn a_root_config_that_isnt_utf8_counts_as_invalid() {
    let m = manifest(
        &[
            (
                b".gitcko/config.toml",
                "100644",
                b"schema_version = 1\nignore = [\"\xff\"]\n",
            ),
            (
                b"pkg/.gitcko/config.toml",
                "100644",
                b"ignore = [\"*.rs\"]\n",
            ),
            (b"pkg/a.rs", "100644", b"fn a() {}\n"),
        ],
        &IndexLimits::default(),
    );
    assert!(m.config_invalid);
    let a = m.entries.iter().find(|e| e.path == "pkg/a.rs").unwrap();
    assert!(
        !a.ignored_by_config,
        "a child config must not apply without its root"
    );
}

#[test]
fn an_executable_config_file_is_still_config() {
    let m = manifest(
        &[
            (
                b".gitcko/config.toml",
                "100755",
                b"schema_version = 1\nignore = [\"*.gen.rs\"]\n",
            ),
            (b"a.gen.rs", "100644", b"fn a() {}\n"),
        ],
        &IndexLimits::default(),
    );
    assert!(
        m.entries
            .iter()
            .find(|e| e.path == "a.gen.rs")
            .unwrap()
            .ignored_by_config
    );
}

#[test]
fn display_paths_never_collide() {
    // A name that isn't UTF-8 and a valid name spelling out the same escape.
    let m = manifest(
        &[
            (b"bad\xff.rs", "100644", b"x"),
            (b"bad\\xff.rs", "100644", b"x"),
        ],
        &IndexLimits::default(),
    );
    let paths: std::collections::HashSet<&str> =
        m.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths.len(), 2, "{paths:?}");
}
