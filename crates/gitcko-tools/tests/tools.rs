// SPDX-License-Identifier: AGPL-3.0-or-later

//! The static review tools over a two-commit fixture (REVIEW_ENGINE §10).

use gitcko_domain::{RepoPath, Side};
use gitcko_git::Repo;
use gitcko_index::{IndexLimits, Revision, build};
use gitcko_test_support::{TempDir, empty_repo, literal_commit};
use gitcko_tools::{Limits, NAMES, Status, ToolOutput, Tools, definitions};
use proptest::prelude::*;
use serde_json::json;

const LIB_BASE: &[u8] =
    b"mod util;\n\npub fn total(xs: &[u32]) -> u32 {\n    util::helper(xs)\n}\n";
const LIB_HEAD: &[u8] =
    b"mod util;\n\npub fn total(xs: &[u32]) -> u32 {\n    util::helper(xs) + 1\n}\n\npub fn twice(xs: &[u32]) -> u32 {\n    total(xs) * 2\n}\n";
const UTIL: &[u8] = b"pub fn helper(xs: &[u32]) -> u32 {\n    xs.iter().sum()\n}\n";

struct Fixture {
    repo: Repo,
    index: Revision,
    change: gitcko_diff::ChangeSet,
    base: gitcko_git::CommitId,
    head: gitcko_git::CommitId,
    _dir: TempDir,
}

fn fixture() -> Fixture {
    let dir = empty_repo("tools");
    // A credential-shaped canary, built at run time so no literal secret is committed.
    let secret = format!("const KEY: &str = \"{}\";\n", gitcko_redact::canaries()[1]);
    let cargo: &[u8] = b"[package]\nname = \"demo\"\n";
    let base = literal_commit(
        dir.path(),
        &[
            (b"Cargo.toml", "100644", cargo),
            (b"src/lib.rs", "100644", LIB_BASE),
            (b"src/util.rs", "100644", UTIL),
        ],
    );
    let head = literal_commit(
        dir.path(),
        &[
            (b"Cargo.toml", "100644", cargo),
            (b"src/lib.rs", "100644", LIB_HEAD),
            (b"src/util.rs", "100644", UTIL),
            (b"src/keys.rs", "100644", secret.as_bytes()),
            (b".env", "100644", b"HELPER=1\n"),
            (b"deploy/id_rsa", "100644", b"helper\n"),
            (b"data.bin", "100644", b"helper\0\x01"),
            (b"link", "120000", b"../../outside"),
            (b"notes.md", "100644", b"helper notes\n"),
        ],
    );
    let repo = Repo::open(dir.path()).unwrap();
    let (base, head) = (repo.resolve(&base).unwrap(), repo.resolve(&head).unwrap());
    let change = repo.diff(base, head).unwrap();
    let index = build(&repo, head, &IndexLimits::default()).unwrap();
    Fixture {
        repo,
        index,
        change,
        base,
        head,
        _dir: dir,
    }
}

impl Fixture {
    fn tools(&self, limits: Limits) -> Tools<'_> {
        Tools::new(
            &self.repo,
            &self.index,
            &self.change,
            self.base,
            self.head,
            limits,
        )
    }
}

fn call(tools: &mut Tools, name: &str, args: serde_json::Value) -> ToolOutput {
    tools.run_raw(name, &args.to_string())
}

#[test]
fn read_file_returns_numbered_lines_and_records_them_as_evidence() {
    let f = fixture();
    let mut tools = f.tools(Limits::default());
    let out = call(
        &mut tools,
        "read_file",
        json!({"path": "src/lib.rs", "start_line": 3, "end_line": 5}),
    );
    assert_eq!(out.status, Status::Ok);
    assert!(!out.truncated);
    assert_eq!(
        out.body["lines"],
        "    3| pub fn total(xs: &[u32]) -> u32 {\n    4|     util::helper(xs) + 1\n    5| }\n"
    );
    let id = out.body["evidence"].as_str().unwrap();
    let e = tools.ledger().get(id).unwrap();
    assert_eq!(
        (e.range.side(), e.range.start(), e.range.end()),
        (Side::New, 3, 5)
    );
    assert_eq!(
        e.text,
        "pub fn total(xs: &[u32]) -> u32 {\n    util::helper(xs) + 1\n}"
    );
    // The base side is the old file, and reading the same range twice keeps one id.
    let old = call(
        &mut tools,
        "read_file",
        json!({"path": "src/lib.rs", "side": "old", "start_line": 4, "end_line": 4}),
    );
    assert_eq!(old.body["lines"], "    4|     util::helper(xs)\n");
    let again = call(
        &mut tools,
        "read_file",
        json!({"path": "src/lib.rs", "start_line": 3, "end_line": 5}),
    );
    assert_eq!(again.body["evidence"], id);
    assert_eq!(tools.ledger().entries().len(), 2);
}

#[test]
fn read_file_clamps_to_the_file_and_the_read_limit() {
    let f = fixture();
    let mut tools = f.tools(Limits {
        read_lines: 2,
        ..Limits::default()
    });
    let out = call(
        &mut tools,
        "read_file",
        json!({"path": "src/lib.rs", "start_line": 8}),
    );
    assert_eq!(
        (out.body["start_line"].clone(), out.body["end_line"].clone()),
        (json!(8), json!(9))
    );
    assert!(!out.truncated, "only lines 8-9 were left");
    let out = call(&mut tools, "read_file", json!({"path": "src/lib.rs"}));
    assert_eq!(out.body["end_line"], 2);
    assert!(out.truncated, "the read limit cut it short");
    let past = call(
        &mut tools,
        "read_file",
        json!({"path": "src/lib.rs", "start_line": 99}),
    );
    assert_eq!(past.status, Status::Ok);
    assert_eq!(past.body["lines"], "");
    assert_eq!(past.body.get("evidence"), None, "nothing was observed");
}

#[test]
fn escaping_paths_are_refused_and_never_read() {
    let f = fixture();
    let mut tools = f.tools(Limits::default());
    for path in [
        "../src/lib.rs",
        "/etc/passwd",
        "src/../../x",
        "",
        "src//lib.rs",
        ".git/config",
        "C:/x",
    ] {
        let out = call(&mut tools, "read_file", json!({ "path": path }));
        assert!(
            matches!(out.status, Status::Invalid | Status::NotFound),
            "{path:?}: {:?}",
            out.status
        );
    }
    assert!(tools.ledger().entries().is_empty());
}

#[test]
fn links_binaries_and_missing_files_are_told_apart() {
    let f = fixture();
    let mut tools = f.tools(Limits::default());
    let status = |tools: &mut Tools, path: &str| {
        let out = call(tools, "read_file", json!({ "path": path }));
        (out.status, out.body["reason"].as_str().map(str::to_string))
    };
    assert_eq!(
        status(&mut tools, "link"),
        (Status::Unavailable, Some("symlink".into()))
    );
    assert_eq!(
        status(&mut tools, "data.bin"),
        (Status::Unavailable, Some("binary".into()))
    );
    assert_eq!(
        status(&mut tools, "nope.rs"),
        (Status::NotFound, Some("no_such_file".into()))
    );
    // Added at head: not on the base side.
    let out = call(
        &mut tools,
        "read_file",
        json!({"path": "notes.md", "side": "old"}),
    );
    assert_eq!(out.status, Status::NotFound);
}

#[test]
fn secret_looking_files_are_held_back_from_every_tool() {
    let f = fixture();
    let mut tools = f.tools(Limits::default());
    for path in [".env", "deploy/id_rsa", "src/keys.rs"] {
        let out = call(&mut tools, "read_file", json!({ "path": path }));
        assert_eq!(out.status, Status::HeldBack, "{path}");
        let diff = call(&mut tools, "read_diff", json!({ "path": path }));
        assert_eq!(diff.status, Status::HeldBack, "{path}");
    }
    // `helper` appears in .env, id_rsa, notes.md and util.rs: the search shows the last
    // two (the positive control) and counts what it held back (the index skips some
    // dotfiles itself, so at least one).
    let out = call(&mut tools, "search_code", json!({"query": "helper"}));
    let paths: Vec<&str> = out.body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"notes.md") && paths.contains(&"src/util.rs"),
        "{paths:?}"
    );
    assert!(
        !paths.iter().any(|p| [".env", "deploy/id_rsa"].contains(p)),
        "{paths:?}"
    );
    assert!(out.body["held_back"].as_u64().unwrap() >= 1);
    let canary = &gitcko_redact::canaries()[1];
    for e in tools.ledger().entries() {
        assert!(!e.text.contains(canary.as_str()));
    }
}

#[test]
fn calls_outside_the_contract_are_refused_without_echoing_them() {
    let f = fixture();
    let mut tools = f.tools(Limits::default());
    let canary = gitcko_redact::canaries()[1].clone();
    let refused = [
        tools.run_raw("run_command", &json!({"cmd": canary}).to_string()),
        tools.run_raw("git_blame", &json!({"path": "src/lib.rs"}).to_string()),
        tools.run_raw(
            "read_file",
            &json!({"path": "src/lib.rs", "allow_shell": true}).to_string(),
        ),
        tools.run_raw("read_file", "not json"),
        tools.run_raw(
            "read_file",
            &json!({"path": "x".repeat(20_000)}).to_string(),
        ),
        tools.run_raw("read_file", &json!({"path": canary}).to_string()),
    ];
    for out in &refused {
        assert!(
            matches!(out.status, Status::Invalid | Status::NotFound),
            "{:?}",
            out.status
        );
        assert!(!out.to_message().contains(&canary));
    }
    assert_eq!(refused[0].body["reason"], "unknown_tool");
    assert_eq!(refused[1].body["reason"], "unknown_tool");
    assert_eq!(refused[2].body["reason"], "bad_arguments");
    assert_eq!(refused[4].body["reason"], "arguments_too_large");
}

#[test]
fn read_diff_maps_both_sides_of_each_hunk() {
    let f = fixture();
    let mut tools = f.tools(Limits::default());
    let out = call(&mut tools, "read_diff", json!({"path": "src/lib.rs"}));
    assert_eq!(out.status, Status::Ok);
    let hunk = &out.body["hunks"][0];
    let old = tools
        .ledger()
        .get(hunk["old_evidence"].as_str().unwrap())
        .unwrap()
        .clone();
    let new = tools
        .ledger()
        .get(hunk["new_evidence"].as_str().unwrap())
        .unwrap()
        .clone();
    assert_eq!(old.range.side(), Side::Old);
    assert_eq!(new.range.side(), Side::New);
    // Each side's evidence is exactly those lines of that side's file.
    for (e, file) in [(&old, LIB_BASE), (&new, LIB_HEAD)] {
        let text = std::str::from_utf8(file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let want = lines[(e.range.start() - 1) as usize..e.range.end() as usize].join("\n");
        assert_eq!(e.text, want);
    }
    let unchanged = call(&mut tools, "read_diff", json!({"path": "src/util.rs"}));
    assert_eq!(unchanged.status, Status::NotFound);
}

#[test]
fn symbol_tools_chain_through_ids_and_refuse_unknown_ones() {
    let f = fixture();
    let mut tools = f.tools(Limits::default());
    let found = call(&mut tools, "find_symbol", json!({"name": "total"}));
    let total = found.body["symbols"][0]["symbol"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(found.body["symbols"][0]["path"], "src/lib.rs");
    let callers = call(&mut tools, "find_callers", json!({ "symbol": total }));
    assert_eq!(callers.status, Status::Ok);
    let from: Vec<&str> = callers.body["resolved"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["from"].as_str())
        .collect();
    let twice = call(&mut tools, "find_symbol", json!({"name": "twice"}));
    assert_eq!(from, [twice.body["symbols"][0]["symbol"].as_str().unwrap()]);
    for bad in ["s999999", "total", "s-1", ""] {
        let out = call(&mut tools, "find_callers", json!({ "symbol": bad }));
        assert_eq!(out.status, Status::NotFound, "{bad:?}");
    }
}

#[test]
fn history_and_repo_facts_are_bounded_and_counted() {
    let f = fixture();
    let mut tools = f.tools(Limits::default());
    let log = call(&mut tools, "git_log", json!({}));
    assert_eq!(
        log.body["commits"].as_array().unwrap().len(),
        1,
        "literal commits have no parent"
    );
    let summary = call(&mut tools, "retrieve_repo_summary", json!({}));
    assert_eq!(summary.body["crates"], json!(["demo"]));
    let chunks = call(
        &mut tools,
        "retrieve_chunks",
        json!({"query": "helper sum"}),
    );
    assert_eq!(chunks.status, Status::Ok);
    assert!(!chunks.body["chunks"].as_array().unwrap().is_empty());
}

#[test]
fn every_offered_tool_has_a_closed_schema() {
    let tools = definitions();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, NAMES);
    for t in &tools {
        assert_eq!(t.parameters["additionalProperties"], false, "{}", t.name);
    }
}

#[test]
fn any_path_the_model_sends_reads_only_tree_files() {
    // One fixture for every case: building the index per case would take minutes.
    let f = fixture();
    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(
            &prop_oneof![
                Just("src/lib.rs".to_string()),
                Just("notes.md".to_string()),
                "[a-z./\\\\:]{0,24}",
            ],
            |path| {
                let mut tools = f.tools(Limits::default());
                let out = call(&mut tools, "read_file", json!({ "path": path }));
                if out.status == Status::Ok {
                    let p = RepoPath::parse(&path).unwrap();
                    prop_assert!(f.repo.read_blob(f.head, &p).unwrap().is_some());
                }
                Ok(())
            },
        )
        .unwrap();
}

#[test]
fn retrieved_chunks_hold_back_secret_content() {
    let f = fixture();
    let mut tools = f.tools(Limits::default());
    let out = call(&mut tools, "retrieve_chunks", json!({"query": "KEY"}));
    assert_eq!(out.status, Status::Ok);
    let paths: Vec<&str> = out.body["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert!(!paths.contains(&"src/keys.rs"), "{paths:?}");
    assert!(
        out.body["held_back"].as_u64().unwrap() >= 1,
        "the index did match it"
    );
    assert!(
        !out.to_message()
            .contains(gitcko_redact::canaries()[1].as_str())
    );
}
