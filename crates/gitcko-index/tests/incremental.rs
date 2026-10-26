// SPDX-License-Identifier: AGPL-3.0-or-later

//! Incremental builds (INDEXING §23): a revision built from the previous one equals a
//! clean build of the same commit, whatever changed in between.

use std::collections::BTreeMap;

use gitcko_domain::{Coverage, Reason};
use gitcko_git::Repo;
use gitcko_index::{IndexLimits, Revision, build, build_incremental};
use gitcko_search::{Query, QueryMode};
use gitcko_test_support::{TempDir, empty_repo, literal_commit};
use proptest::prelude::*;

type Files = BTreeMap<String, String>;

fn files(list: &[(&str, &str)]) -> Files {
    list.iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect()
}

/// Commits `files` to the repository at `dir` and opens it.
fn commit(dir: &TempDir, files: &Files) -> (Repo, gitcko_git::CommitId) {
    let list: Vec<(&[u8], &str, &[u8])> = files
        .iter()
        .map(|(p, t)| (p.as_bytes(), "100644", t.as_bytes()))
        .collect();
    let id = literal_commit(dir.path(), &list);
    let repo = Repo::open(dir.path()).unwrap();
    let commit = repo.resolve(&id).unwrap();
    (repo, commit)
}

fn clean(dir: &TempDir, files: &Files) -> Revision {
    let (repo, commit) = commit(dir, files);
    build(&repo, commit, &IndexLimits::default()).unwrap()
}

fn incremental(dir: &TempDir, files: &Files, previous: &Revision) -> Revision {
    let (repo, commit) = commit(dir, files);
    build_incremental(&repo, commit, &IndexLimits::default(), previous).unwrap()
}

/// Everything a reader can see in a revision: every layer, the search documents, and a
/// fixed set of queries.
fn visible(rev: &Revision) -> String {
    let mut out = format!(
        "{}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n",
        rev.commit,
        rev.manifest,
        rev.parsed,
        rev.graph,
        rev.chunks,
        rev.coverage,
        rev.versions,
        rev.lexical.digests().unwrap()
    );
    for q in ["helper", "total", "thing", "run", "f0", "f1"] {
        let hits = rev
            .lexical
            .search(&Query::new(QueryMode::Identifier, q), 20, 0)
            .unwrap();
        let ids: Vec<u64> = hits.hits.iter().map(|h| h.id).collect();
        out += &format!("{q}: {ids:?} of {}\n", hits.total);
    }
    out
}

fn base() -> Files {
    files(&[
        (
            "Cargo.toml",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        ),
        (
            "src/lib.rs",
            "mod api;\nmod util;\npub use util::helper;\n\npub fn total(xs: &[u32]) -> u32 {\n    helper(xs)\n}\n",
        ),
        (
            "src/util.rs",
            "pub fn helper(xs: &[u32]) -> u32 {\n    xs.iter().sum()\n}\n",
        ),
        (
            "src/api.rs",
            "use crate::util::helper;\nuse crate::missing::thing;\n\npub fn run() -> u32 {\n    thing();\n    helper(&[1, 2])\n}\n",
        ),
        ("README.md", "# Demo\n\nCall total.\n"),
        (".gitcko/config.toml", "schema_version = 1\nignore = []\n"),
    ])
}

/// Applies `edit` to the base workspace and checks an incremental build from the base's
/// revision equals a clean build.
fn assert_incremental_equals_clean(edit: impl Fn(&mut Files)) {
    let dir = empty_repo("incremental");
    let before = base();
    let previous = clean(&dir, &before);
    let mut after = before.clone();
    edit(&mut after);
    let inc = incremental(&dir, &after, &previous);
    let full = clean(&dir, &after);
    assert_eq!(visible(&inc), visible(&full));
}

fn set(f: &mut Files, path: &str, text: &str) {
    f.insert(path.to_string(), text.to_string());
}

#[test]
fn adding_a_file() {
    assert_incremental_equals_clean(|f| {
        set(f, "src/extra.rs", "pub fn extra() -> u32 {\n    1\n}\n");
        let lib = f["src/lib.rs"].replace("mod util;", "mod util;\nmod extra;");
        set(f, "src/lib.rs", &lib);
    });
}

#[test]
fn modifying_a_file() {
    assert_incremental_equals_clean(|f| {
        set(
            f,
            "src/util.rs",
            "pub fn helper(xs: &[u32]) -> u32 {\n    xs.iter().copied().max().unwrap_or(0)\n}\n",
        );
    });
}

#[test]
fn deleting_a_file() {
    assert_incremental_equals_clean(|f| {
        f.remove("src/api.rs");
        let lib = f["src/lib.rs"].replace("mod api;\n", "");
        set(f, "src/lib.rs", &lib);
    });
}

#[test]
fn renaming_files() {
    assert_incremental_equals_clean(|f| {
        let readme = f.remove("README.md").unwrap();
        set(f, "docs/README.md", &readme);
        let api = f.remove("src/api.rs").unwrap();
        set(f, "src/api/mod.rs", &api);
    });
}

#[test]
fn moving_a_symbol() {
    assert_incremental_equals_clean(|f| {
        set(f, "src/util.rs", "pub fn other() {}\n");
        let api = f["src/api.rs"].replace("use crate::util::helper;\n", "")
            + "\npub fn helper(xs: &[u32]) -> u32 {\n    xs.iter().sum()\n}\n";
        set(f, "src/api.rs", &api);
        let lib = f["src/lib.rs"].replace("pub use util::helper;", "pub use api::helper;");
        set(f, "src/lib.rs", &lib);
    });
}

#[test]
fn changing_an_import_target() {
    assert_incremental_equals_clean(|f| {
        let api = f["src/api.rs"].replace("use crate::util::helper;", "use crate::helper;");
        set(f, "src/api.rs", &api);
    });
}

#[test]
fn adding_a_missing_module_an_unchanged_file_uses() {
    assert_incremental_equals_clean(|f| {
        set(f, "src/missing.rs", "pub fn thing() {}\n");
        let lib = f["src/lib.rs"].replace("mod api;", "mod api;\nmod missing;");
        set(f, "src/lib.rs", &lib);
    });
}

#[test]
fn adding_a_competing_declaration() {
    assert_incremental_equals_clean(|f| {
        set(
            f,
            "src/more.rs",
            "pub fn helper(xs: &[u32]) -> u32 {\n    0\n}\n",
        );
        let lib = f["src/lib.rs"].replace("mod util;", "mod util;\nmod more;");
        set(f, "src/lib.rs", &lib);
        let api = f["src/api.rs"].replace(
            "use crate::util::helper;",
            "use crate::util::*;\nuse crate::more::*;",
        );
        set(f, "src/api.rs", &api);
    });
}

#[test]
fn changing_a_re_export_chain() {
    assert_incremental_equals_clean(|f| {
        set(
            f,
            "src/deep.rs",
            "pub fn helper(xs: &[u32]) -> u32 {\n    xs.len() as u32\n}\n",
        );
        set(f, "src/util.rs", "pub use crate::deep::helper;\n");
        let lib = f["src/lib.rs"].replace("mod util;", "mod util;\nmod deep;");
        set(f, "src/lib.rs", &lib);
    });
}

#[test]
fn changing_ignore_patterns() {
    assert_incremental_equals_clean(|f| {
        set(
            f,
            ".gitcko/config.toml",
            "schema_version = 1\nignore = [\"src/api.rs\", \"*.md\"]\n",
        );
    });
}

/// The parse that `build_incremental` would reuse for `path`, marked so a test can tell
/// a reused parse from a fresh one.
fn mark(rev: &mut Revision, path: &str) {
    let entry = rev
        .manifest
        .entries
        .iter()
        .position(|e| e.repo_path.as_ref().is_some_and(|p| p.as_str() == path))
        .unwrap();
    let parsed = rev.parsed.iter_mut().find(|p| p.entry == entry).unwrap();
    parsed.extraction.symbols[0].name = "marked".to_string();
}

fn has_mark(rev: &Revision) -> bool {
    rev.graph.symbols.iter().any(|s| s.name == "marked")
}

#[test]
fn an_unchanged_file_is_not_parsed_again_and_a_changed_one_is() {
    let dir = empty_repo("incremental");
    let mut previous = clean(&dir, &base());
    mark(&mut previous, "src/util.rs");
    let mut after = base();
    // Only the README changes: util.rs's parse is reused, mark and all.
    set(&mut after, "README.md", "# Demo\n\nChanged.\n");
    assert!(has_mark(&incremental(&dir, &after, &previous)));
    // Now util.rs changes too: it's parsed again.
    set(
        &mut after,
        "src/util.rs",
        "pub fn helper(xs: &[u32]) -> u32 {\n    0\n}\n",
    );
    assert!(!has_mark(&incremental(&dir, &after, &previous)));
}

#[test]
fn a_renamed_file_keeps_its_parse() {
    let dir = empty_repo("incremental");
    let mut previous = clean(&dir, &base());
    mark(&mut previous, "src/api.rs");
    let mut after = base();
    let api = after.remove("src/api.rs").unwrap();
    set(&mut after, "src/api/mod.rs", &api);
    assert!(has_mark(&incremental(&dir, &after, &previous)));
}

#[test]
fn nothing_is_reused_from_a_revision_with_other_versions() {
    let dir = empty_repo("incremental");
    let mut previous = clean(&dir, &base());
    mark(&mut previous, "src/util.rs");
    // As if the Rust adapter's version had been bumped since.
    previous.versions.adapters = vec!["rust-0+old".to_string()];
    let inc = incremental(&dir, &base(), &previous);
    assert!(!has_mark(&inc));
    assert_eq!(visible(&inc), visible(&clean(&dir, &base())));
}

#[test]
fn a_partial_parse_is_parsed_again() {
    // A parse cut short, by a timeout say, may come out complete next time. Either
    // coverage being partial counts.
    for references in [false, true] {
        let dir = empty_repo("incremental");
        let mut previous = clean(&dir, &base());
        mark(&mut previous, "src/util.rs");
        let parsed = previous
            .parsed
            .iter_mut()
            .find(|p| p.extraction.symbols.iter().any(|s| s.name == "marked"))
            .unwrap();
        let partial = Coverage::Partial {
            reason: Reason::new("parse_timeout").unwrap(),
            omitted: None,
        };
        if references {
            parsed.extraction.reference_coverage = partial;
        } else {
            parsed.extraction.coverage = partial;
        }
        assert!(
            !has_mark(&incremental(&dir, &base(), &previous)),
            "{references}"
        );
    }
}

/// One module of the generated workspace: which other modules its function calls, and
/// whether it lives at `src/m<i>.rs` or `src/m<i>/mod.rs`.
#[derive(Debug, Clone)]
struct Module {
    calls: Vec<usize>,
    nested: bool,
    body: u8,
}

const MODULES: usize = 6;

#[derive(Debug, Clone)]
enum Edit {
    /// Adds the module, or replaces it.
    Put(usize, Module),
    Delete(usize),
    /// Moves it between `m<i>.rs` and `m<i>/mod.rs`.
    Move(usize),
    /// Ignores one module's file through `.gitcko/config.toml`, or no file.
    Ignore(Option<usize>),
    Readme(u8),
}

fn module() -> impl Strategy<Value = Module> {
    (
        proptest::collection::vec(0..MODULES, 0..3),
        any::<bool>(),
        any::<u8>(),
    )
        .prop_map(|(calls, nested, body)| Module {
            calls,
            nested,
            body,
        })
}

fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        (0..MODULES, module()).prop_map(|(i, m)| Edit::Put(i, m)),
        (0..MODULES).prop_map(Edit::Delete),
        (0..MODULES).prop_map(Edit::Move),
        proptest::option::of(0..MODULES).prop_map(Edit::Ignore),
        any::<u8>().prop_map(Edit::Readme),
    ]
}

#[derive(Debug, Clone, Default)]
struct Workspace {
    modules: BTreeMap<usize, Module>,
    ignored: Option<usize>,
    readme: u8,
}

impl Workspace {
    fn path(i: usize, m: &Module) -> String {
        if m.nested {
            format!("src/m{i}/mod.rs")
        } else {
            format!("src/m{i}.rs")
        }
    }

    fn apply(&mut self, edit: &Edit) {
        match edit {
            Edit::Put(i, m) => {
                self.modules.insert(*i, m.clone());
            }
            Edit::Delete(i) => {
                self.modules.remove(i);
            }
            Edit::Move(i) => {
                if let Some(m) = self.modules.get_mut(i) {
                    m.nested = !m.nested;
                }
            }
            Edit::Ignore(i) => self.ignored = *i,
            Edit::Readme(n) => self.readme = *n,
        }
    }

    /// The files: a crate root declaring every module, and each module calling others
    /// (some of which may not exist: unresolved until they're added).
    fn files(&self) -> Files {
        let mut out = Files::new();
        out.insert(
            "Cargo.toml".into(),
            "[package]\nname = \"gen\"\nversion = \"0.1.0\"\n".into(),
        );
        let mut lib = String::new();
        for (i, m) in &self.modules {
            lib += &format!("mod m{i};\n");
            let mut text = String::new();
            for c in &m.calls {
                text += &format!("use crate::m{c}::f{c};\n");
            }
            text += &format!("\npub fn f{i}() -> u32 {{\n");
            for c in &m.calls {
                text += &format!("    f{c}();\n");
            }
            text += &format!("    {}\n}}\n", m.body);
            out.insert(Self::path(*i, m), text);
        }
        out.insert("src/lib.rs".into(), lib);
        out.insert("README.md".into(), format!("# Gen {}\n", self.readme));
        let ignore = match self
            .ignored
            .and_then(|i| Some(Self::path(i, self.modules.get(&i)?)))
        {
            Some(p) => format!("[\"{p}\"]"),
            None => "[]".into(),
        };
        out.insert(
            ".gitcko/config.toml".into(),
            format!("schema_version = 1\nignore = {ignore}\n"),
        );
        out
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    /// Each revision in a chain of incremental builds equals a clean build of its commit
    /// (INDEXING §23.6, §40 box 18).
    #[test]
    fn a_chain_of_incremental_builds_equals_clean_builds(
        start in proptest::collection::vec(module(), 0..MODULES),
        edits in proptest::collection::vec(edit(), 1..5),
    ) {
        let dir = empty_repo("incremental-prop");
        let mut ws = Workspace::default();
        for (i, m) in start.into_iter().enumerate() {
            ws.modules.insert(i, m);
        }
        let mut previous = clean(&dir, &ws.files());
        for e in &edits {
            ws.apply(e);
            let files = ws.files();
            let inc = incremental(&dir, &files, &previous);
            prop_assert_eq!(visible(&inc), visible(&clean(&dir, &files)), "after {:?}", e);
            previous = inc;
        }
    }
}
