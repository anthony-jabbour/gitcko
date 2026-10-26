// SPDX-License-Identifier: AGPL-3.0-or-later

//! The graph over a synthetic Rust workspace (INDEXING §38.3–38.4).

use gitcko_domain::RepoPath;
use gitcko_graph::{
    Edge, EdgeKind, Endpoint, FileInput, Graph, GraphConfidence, GraphInput, Resolution, Target,
    build,
};
use gitcko_syntax::{Extraction, Language, ParseLimits, adapter};
use proptest::prelude::*;

/// A two-crate workspace: `my-core` (lib, with a module tree, re-exports, traits and
/// tests) and `app` (a binary using it). Every file is synthetic.
const WORKSPACE: &[(&str, &str)] = &[
    ("Cargo.toml", "[workspace]\nmembers = [\"core\", \"app\"]\n"),
    (
        "core/Cargo.toml",
        "[package]\nname = \"my-core\"\nversion = \"0.1.0\"\n",
    ),
    (
        "core/src/lib.rs",
        "pub mod shapes;\nmod util;\npub use shapes::Circle;\n\n\
         pub trait Area: Describe {\n    fn area(&self) -> f64;\n}\n\n\
         pub trait Describe {\n    fn describe(&self) -> String;\n}\n\n\
         pub fn total(xs: &[Circle]) -> f64 {\n    xs.iter().map(|c| c.area()).sum()\n}\n\n\
         #[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn totals() {\n        assert_eq!(total(&[]), 0.0);\n    }\n}\n",
    ),
    (
        "core/src/shapes.rs",
        "use crate::{Area, Describe};\n\n\
         pub struct Circle {\n    pub r: f64,\n}\n\n\
         impl Area for Circle {\n    fn area(&self) -> f64 {\n        crate::util::pi() * self.r\n    }\n}\n\n\
         impl Describe for Circle {\n    fn describe(&self) -> String {\n        String::new()\n    }\n}\n",
    ),
    ("core/src/util.rs", "pub fn pi() -> f64 {\n    3.14\n}\n"),
    (
        "core/tests/integration.rs",
        "use my_core::total;\n\n#[test]\nfn it() {\n    total(&[]);\n}\n",
    ),
    (
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nmy-core = { path = \"../core\" }\n",
    ),
    (
        "app/src/main.rs",
        "use my_core::{total, Circle};\nuse serde::Serialize;\n\n\
         fn main() {\n    let c = Circle { r: 1.0 };\n    run(total(&[c]));\n    shape().area();\n}\n\n\
         fn run(_: f64) {}\n",
    ),
];

struct Built {
    graph: Graph,
}

fn build_from(files: &[(&str, &str)]) -> Built {
    let rust = adapter(Language::Rust).unwrap();
    let extracted: Vec<(RepoPath, Extraction)> = files
        .iter()
        .filter(|(p, _)| p.ends_with(".rs"))
        .map(|(p, src)| {
            (
                RepoPath::parse(p).unwrap(),
                rust.extract(src, &ParseLimits::default()),
            )
        })
        .collect();
    let input = GraphInput {
        files: extracted
            .iter()
            .map(|(path, extraction)| FileInput {
                path: path.clone(),
                extraction,
            })
            .collect(),
        manifests: files
            .iter()
            .filter(|(p, _)| p.ends_with("Cargo.toml"))
            .map(|(p, text)| (RepoPath::parse(p).unwrap(), *text))
            .collect(),
    };
    Built {
        graph: build(&input),
    }
}

fn endpoint(g: &Graph, e: &Endpoint) -> String {
    match e {
        Endpoint::File(f) => format!("file:{}", g.files[*f].path.as_str()),
        Endpoint::Module(c, path) => format!("mod:{}::{}", g.crates[*c].name, path.join("::")),
        Endpoint::Symbol(s) => g.symbols[*s].qualified_name.clone(),
    }
}

fn target(g: &Graph, t: &Target) -> String {
    match t {
        Target::Resolved(e) => endpoint(g, e),
        Target::Candidates(c) => format!(
            "one of [{}]",
            c.iter()
                .map(|e| endpoint(g, e))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Target::Unresolved(path) => format!("? {}", path.join("::")),
    }
}

fn render(g: &Graph) -> String {
    let mut out = String::new();
    for c in &g.crates {
        out += &format!("crate {} {} root={}\n", c.target, c.name, c.root.as_str());
    }
    for f in &g.files {
        let unit = f
            .crate_unit
            .map(|c| g.crates[c].name.clone())
            .unwrap_or("-".into());
        out += &format!(
            "file {} crate={} mod=[{}]{}\n",
            f.path.as_str(),
            unit,
            f.module.join("::"),
            if f.module_inferred { " inferred" } else { "" }
        );
    }
    for s in &g.symbols {
        out += &format!(
            "sym {:?} {}{}\n",
            s.kind,
            s.qualified_name,
            s.discriminator
                .as_deref()
                .map(|d| format!(" <{d}>"))
                .unwrap_or_default()
        );
    }
    for e in &g.edges {
        if matches!(e.kind, EdgeKind::Contains | EdgeKind::Declares) {
            continue; // structural, checked separately
        }
        let lim: Vec<&str> = e.limitations.iter().map(|r| r.as_str()).collect();
        out += &format!(
            "{:?}: {} -> {} [{:?}, {:?}]{}\n",
            e.kind,
            endpoint(g, &e.from),
            target(g, &e.to),
            e.resolution,
            e.confidence,
            if lim.is_empty() {
                String::new()
            } else {
                format!(" ({})", lim.join(","))
            },
        );
    }
    for (kind, coverage) in &g.coverage {
        out += &format!("coverage {kind:?}: {coverage:?}\n");
    }
    out
}

#[test]
fn workspace_graph() {
    insta::assert_snapshot!(render(&build_from(WORKSPACE).graph));
}

fn edges_of(g: &Graph, kind: EdgeKind) -> Vec<&Edge> {
    g.edges.iter().filter(|e| e.kind == kind).collect()
}

fn find<'g>(g: &'g Graph, kind: EdgeKind, from: &str, to: &str) -> Option<&'g Edge> {
    edges_of(g, kind)
        .into_iter()
        .find(|e| endpoint(g, &e.from) == from && target(g, &e.to) == to)
}

#[test]
fn a_re_export_resolves_to_the_declaration_it_names() {
    let g = build_from(WORKSPACE).graph;
    let e = find(
        &g,
        EdgeKind::Imports,
        "file:app/src/main.rs",
        "my_core::shapes::Circle",
    )
    .expect("import through `pub use`");
    assert_eq!(e.resolution, Resolution::InferredStatic);
}

#[test]
fn a_method_call_on_an_unknown_receiver_lists_every_candidate() {
    let g = build_from(WORKSPACE).graph;
    let e = edges_of(&g, EdgeKind::Calls)
        .into_iter()
        .find(|e| endpoint(&g, &e.from) == "app::main" && target(&g, &e.to).starts_with("one of"))
        .expect("`shape().area()` stays a candidate");
    assert_eq!(e.resolution, Resolution::CandidateAmbiguous);
    let Target::Candidates(c) = &e.to else {
        unreachable!()
    };
    assert!(c.len() >= 2, "trait declaration and impl: {c:?}");
    assert!(
        e.limitations
            .iter()
            .any(|r| r.as_str() == "method_receiver_unknown")
    );
}

#[test]
fn an_external_crate_is_unresolved_and_says_why() {
    let g = build_from(WORKSPACE).graph;
    let e = find(
        &g,
        EdgeKind::Imports,
        "file:app/src/main.rs",
        "? serde::Serialize",
    )
    .expect("external import kept");
    assert_eq!(e.resolution, Resolution::Unknown);
    assert!(e.limitations.iter().any(|r| r.as_str() == "external_crate"));
}

#[test]
fn implements_extends_and_tests_edges_are_found() {
    let g = build_from(WORKSPACE).graph;
    assert!(
        find(
            &g,
            EdgeKind::Implements,
            "my_core::shapes::Circle",
            "my_core::Area"
        )
        .is_some()
    );
    assert!(
        find(
            &g,
            EdgeKind::Implements,
            "my_core::shapes::Circle",
            "my_core::Describe"
        )
        .is_some()
    );
    assert!(find(&g, EdgeKind::Extends, "my_core::Area", "my_core::Describe").is_some());
    // A glob import (`use super::*`) inside the test module reaches `total`.
    assert!(
        find(
            &g,
            EdgeKind::Tests,
            "my_core::tests::totals",
            "my_core::total"
        )
        .is_some()
    );
    // An integration test is its own crate, depending on `my_core` by name.
    assert!(find(&g, EdgeKind::Tests, "integration::it", "my_core::total").is_some());
}

#[test]
fn no_edge_points_at_something_that_isnt_there() {
    // INDEXING §38.4: no dangling exact endpoints; evidence points at real source.
    for files in [
        WORKSPACE.to_vec(),
        WORKSPACE
            .iter()
            .copied()
            .filter(|(p, _)| !p.ends_with("util.rs"))
            .collect(),
    ] {
        let g = build_from(&files).graph;
        let ok = |e: &Endpoint| match e {
            Endpoint::File(f) => *f < g.files.len(),
            Endpoint::Module(c, _) => *c < g.crates.len(),
            Endpoint::Symbol(s) => *s < g.symbols.len(),
        };
        for e in &g.edges {
            assert!(ok(&e.from), "{e:?}");
            match &e.to {
                Target::Resolved(t) => assert!(ok(t), "{e:?}"),
                Target::Candidates(c) => assert!(c.len() >= 2 && c.iter().all(ok), "{e:?}"),
                Target::Unresolved(p) => assert!(!p.is_empty()),
            }
            assert!(e.file < g.files.len());
        }
    }
}

#[test]
fn deleting_a_file_removes_its_symbols_and_edges_to_them() {
    let without: Vec<_> = WORKSPACE
        .iter()
        .copied()
        .filter(|(p, _)| !p.ends_with("util.rs"))
        .collect();
    let g = build_from(&without).graph;
    assert!(
        g.symbols
            .iter()
            .all(|s| !s.qualified_name.ends_with("util::pi"))
    );
    // The call is still there, now unresolved, not pointing at a stale symbol.
    assert!(
        find(
            &g,
            EdgeKind::Calls,
            "my_core::shapes::Circle::area",
            "? crate::util::pi"
        )
        .is_some()
    );
}

#[test]
fn calls_coverage_is_not_complete_when_calls_stay_candidates() {
    let g = build_from(WORKSPACE).graph;
    assert!(!g.coverage[&EdgeKind::Calls].is_complete());
    assert!(g.coverage[&EdgeKind::Contains].is_complete());
}

#[test]
fn a_re_export_cycle_ends_unresolved() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"loop\"\n"),
        ("src/lib.rs", "mod a;\nmod b;\npub use a::X;\n"),
        ("src/a.rs", "pub use crate::b::X;\n"),
        ("src/b.rs", "pub use crate::a::X;\n"),
    ];
    let g = build_from(&files).graph;
    assert!(
        edges_of(&g, EdgeKind::Imports)
            .iter()
            .all(|e| matches!(e.to, Target::Unresolved(_)))
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Moving code within files (blank lines and comments anywhere between items) never
    /// changes a symbol key (INDEXING §38.3).
    #[test]
    fn keys_survive_lines_moving(seed in proptest::collection::vec(0usize..40, 1..6)) {
        let base = build_from(WORKSPACE).graph;
        let moved: Vec<(String, String)> = WORKSPACE
            .iter()
            .map(|(p, src)| {
                if !p.ends_with(".rs") { return (p.to_string(), src.to_string()); }
                let mut lines: Vec<String> = src.lines().map(String::from).collect();
                for &at in &seed {
                    let at = at.min(lines.len());
                    lines.insert(at, "// moved\n".into());
                }
                (p.to_string(), lines.join("\n"))
            })
            .collect();
        let refs: Vec<(&str, &str)> = moved.iter().map(|(p, s)| (p.as_str(), s.as_str())).collect();
        let after = build_from(&refs).graph;
        let keys = |g: &Graph| { let mut k: Vec<_> = g.symbols.iter().map(|s| (s.qualified_name.clone(), s.key.clone())).collect(); k.sort(); k };
        prop_assert_eq!(keys(&base), keys(&after));
    }
}

#[test]
fn a_path_through_an_inherent_impl_reaches_the_method() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"geo\"\n"),
        (
            "src/lib.rs",
            "pub struct Point;\nimpl Point {\n    pub fn new() -> Point { Point }\n}\npub fn make() -> Point {\n    Point::new()\n}\n",
        ),
    ];
    let g = build_from(&files).graph;
    assert!(find(&g, EdgeKind::Calls, "geo::make", "geo::Point::new").is_some());
}

#[test]
fn a_test_calling_a_test_helper_is_not_a_test_of_it() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"t\"\n"),
        (
            "src/lib.rs",
            "pub fn real() {}\n#[cfg(test)]\nmod tests {\n    fn helper() { super::real(); }\n    #[test]\n    fn t() { helper(); super::real(); }\n}\n",
        ),
    ];
    let g = build_from(&files).graph;
    let tests: Vec<String> = edges_of(&g, EdgeKind::Tests)
        .iter()
        .map(|e| target(&g, &e.to))
        .collect();
    assert!(tests.contains(&"t::real".to_string()), "{tests:?}");
    assert!(
        !tests.contains(&"t::tests::helper".to_string()),
        "{tests:?}"
    );
}

#[test]
fn dense_glob_imports_resolve_in_bounded_time() {
    // Twelve modules, each glob-importing all the others: every lookup of a missing name
    // could branch twelve ways at each of sixteen levels without memoising.
    let mut files = vec![(
        "Cargo.toml".to_string(),
        "[package]\nname = \"dense\"\n".to_string(),
    )];
    let mods: Vec<String> = (0..12).map(|i| format!("m{i}")).collect();
    files.push((
        "src/lib.rs".into(),
        mods.iter()
            .map(|m| format!("mod {m};\n"))
            .collect::<String>()
            + "pub fn f() { missing(); }\n",
    ));
    for m in &mods {
        let globs: String = mods
            .iter()
            .filter(|o| *o != m)
            .map(|o| format!("pub use crate::{o}::*;\n"))
            .collect();
        files.push((format!("src/{m}.rs"), globs + "pub fn g() { missing(); }\n"));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let started = std::time::Instant::now();
    let g = build_from(&refs).graph;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(find(&g, EdgeKind::Calls, "dense::f", "? missing").is_some());
}

#[test]
fn modules_nest_in_folders_and_hyphens_become_underscores() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"nest\"\n"),
        ("src/lib.rs", "mod outer;\n"),
        ("src/outer.rs", "mod inner;\n"),
        ("src/outer/inner.rs", "pub fn deep() {}\n"),
        ("tests/my-test.rs", "#[test]\nfn t() {}\n"),
    ];
    let g = build_from(&files).graph;
    let inner = g
        .files
        .iter()
        .find(|f| f.path.as_str() == "src/outer/inner.rs")
        .unwrap();
    assert_eq!(
        (inner.module.clone(), inner.module_inferred),
        (vec!["outer".to_string(), "inner".to_string()], false)
    );
    assert!(
        g.crates
            .iter()
            .any(|c| c.name == "my_test" && c.target == "test")
    );
}

// ---- Review round 1 findings, one test each. ----

fn resolved(g: &Graph, kind: EdgeKind, from: &str) -> Vec<String> {
    edges_of(g, kind)
        .into_iter()
        .filter(|e| endpoint(g, &e.from) == from)
        .map(|e| target(g, &e.to))
        .collect()
}

#[test]
fn same_named_items_in_two_targets_of_one_package_get_different_keys() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"foo\"\n"),
        ("src/lib.rs", "pub fn helper() {}\n"),
        ("src/main.rs", "fn helper() {}\nfn main() {}\n"),
    ];
    let g = build_from(&files).graph;
    let keys: Vec<_> = g
        .symbols
        .iter()
        .filter(|s| s.name == "helper")
        .map(|s| s.key.clone())
        .collect();
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
}

#[test]
fn a_repeated_module_prefix_is_not_a_cycle() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"r\"\n"),
        (
            "src/lib.rs",
            "pub mod a;\npub fn f() { crate::a::alias::Thing(); }\n",
        ),
        (
            "src/a.rs",
            "pub fn Thing() {}\npub mod alias { pub use crate::a::Thing; }\n",
        ),
    ];
    let g = build_from(&files).graph;
    assert!(
        find(&g, EdgeKind::Calls, "r::f", "r::a::Thing").is_some(),
        "{:?}",
        resolved(&g, EdgeKind::Calls, "r::f")
    );
}

#[test]
fn self_in_a_method_means_the_impl_type() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"p\"\n"),
        (
            "src/lib.rs",
            "pub struct Point;\nimpl Point {\n    pub fn new() -> Point { Point }\n    pub fn copy() -> Point { Self::new() }\n}\n",
        ),
    ];
    let g = build_from(&files).graph;
    assert!(
        find(&g, EdgeKind::Calls, "p::Point::copy", "p::Point::new").is_some(),
        "{:?}",
        resolved(&g, EdgeKind::Calls, "p::Point::copy")
    );
}

#[test]
fn every_leading_super_steps_up_a_module() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"s\"\n"),
        (
            "src/lib.rs",
            "pub fn top() {}\nmod a { mod b { fn g() { super::super::top(); } } }\n",
        ),
    ];
    let g = build_from(&files).graph;
    assert!(
        find(&g, EdgeKind::Calls, "s::a::b::g", "s::top").is_some(),
        "{:?}",
        resolved(&g, EdgeKind::Calls, "s::a::b::g")
    );
}

#[test]
fn a_function_local_import_stays_in_its_function() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"l\"\n"),
        (
            "src/lib.rs",
            "mod x { pub fn go() {} }\nmod y { pub fn go() {} }\nfn one() { use crate::x::go as run; run(); }\nfn two() { use crate::y::go as run; run(); }\n",
        ),
    ];
    let g = build_from(&files).graph;
    assert_eq!(resolved(&g, EdgeKind::Calls, "l::one"), ["l::x::go"]);
    assert_eq!(resolved(&g, EdgeKind::Calls, "l::two"), ["l::y::go"]);
}

#[test]
fn a_missing_item_under_a_local_module_is_unresolved_not_external() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"m\"\n"),
        (
            "src/lib.rs",
            "mod util { pub fn kept() {} }\npub fn f() { util::deleted(); }\n",
        ),
    ];
    let g = build_from(&files).graph;
    let e = find(&g, EdgeKind::Calls, "m::f", "? util::deleted").expect("kept as unresolved");
    assert!(
        e.limitations
            .iter()
            .any(|r| r.as_str() == "unresolved_name"),
        "{:?}",
        e.limitations
    );
    assert!(!g.coverage[&EdgeKind::Calls].is_complete());
}

#[test]
fn walk_limits_make_import_impl_and_extends_coverage_partial() {
    use gitcko_syntax::ParseLimits;
    let source = "use a::b;\nuse c::d;\nuse e::f;\npub trait T: U {}\nimpl T for X {}\n";
    let x = adapter(Language::Rust).unwrap().extract(
        source,
        &ParseLimits {
            max_references: 1,
            ..ParseLimits::default()
        },
    );
    let path = RepoPath::parse("src/lib.rs").unwrap();
    let g = build(&GraphInput {
        files: vec![FileInput {
            path,
            extraction: &x,
        }],
        manifests: vec![(
            RepoPath::parse("Cargo.toml").unwrap(),
            "[package]\nname = \"w\"\n",
        )],
    });
    assert!(
        !g.coverage[&EdgeKind::Imports].is_complete(),
        "{:?}",
        g.coverage
    );
}

#[test]
fn a_file_shared_by_two_targets_belongs_to_both() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"sh\"\n"),
        ("src/lib.rs", "mod shared;\n"),
        (
            "src/main.rs",
            "mod shared;\nfn main() { crate::shared::util(); }\n",
        ),
        ("src/shared.rs", "pub fn util() {}\n"),
    ];
    let g = build_from(&files).graph;
    let calls = resolved(&g, EdgeKind::Calls, "sh::main");
    assert_eq!(calls, ["sh::shared::util"]);
}

#[test]
fn a_path_attribute_inside_an_inline_module_is_relative_to_that_module() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"pa\"\n"),
        (
            "src/lib.rs",
            "mod outer {\n    #[path = \"actual.rs\"]\n    mod inner;\n}\n",
        ),
        ("src/outer/actual.rs", "pub fn here() {}\n"),
    ];
    let g = build_from(&files).graph;
    let f = g
        .files
        .iter()
        .find(|f| f.path.as_str() == "src/outer/actual.rs")
        .unwrap();
    assert_eq!(
        (f.module.clone(), f.module_inferred),
        (vec!["outer".to_string(), "inner".to_string()], false)
    );
}

#[test]
fn folder_binaries_and_explicit_targets_are_found() {
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"t\"\n\n[[test]]\nname = \"spec\"\npath = \"checks/spec.rs\"\n\n[[example]]\nname = \"demo\"\npath = \"demos/demo.rs\"\n",
        ),
        ("src/bin/tool/main.rs", "fn main() {}\n"),
        ("checks/spec.rs", "#[test]\nfn s() {}\n"),
        ("demos/demo.rs", "fn main() {}\n"),
    ];
    let g = build_from(&files).graph;
    let targets: Vec<(String, String)> = g
        .crates
        .iter()
        .map(|c| (c.target.clone(), c.name.clone()))
        .collect();
    for want in [("bin", "tool"), ("test", "spec"), ("example", "demo")] {
        assert!(
            targets.contains(&(want.0.to_string(), want.1.to_string())),
            "{want:?} in {targets:?}"
        );
    }
}

#[test]
fn importing_a_module_is_one_target_not_two() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"mi\"\n"),
        ("src/lib.rs", "mod a {}\nuse crate::a;\n"),
    ];
    let g = build_from(&files).graph;
    let e = edges_of(&g, EdgeKind::Imports).into_iter().next().unwrap();
    assert_eq!(
        e.resolution,
        Resolution::InferredStatic,
        "{}",
        target(&g, &e.to)
    );
}

// ---- Review round 2 findings. ----

#[test]
fn a_method_call_with_no_known_candidates_keeps_calls_coverage_partial() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"u\"\n"),
        ("src/lib.rs", "pub fn f(x: &str) { x.len(); }\n"),
    ];
    let g = build_from(&files).graph;
    assert!(
        !g.coverage[&EdgeKind::Calls].is_complete(),
        "{:?}",
        g.coverage[&EdgeKind::Calls]
    );
}

#[test]
fn declarations_in_sibling_blocks_get_distinct_occurrence_keys() {
    use gitcko_graph::KeyStability;
    let files = [
        ("Cargo.toml", "[package]\nname = \"b\"\n"),
        ("src/lib.rs", "fn f() { { struct S; } { struct S; } }\n"),
    ];
    let g = build_from(&files).graph;
    let s: Vec<_> = g.symbols.iter().filter(|s| s.name == "S").collect();
    assert_eq!(s.len(), 2);
    assert_ne!(s[0].key, s[1].key);
    assert!(s.iter().all(|s| s.stability == KeyStability::Occurrence));
}

#[test]
fn a_call_through_an_external_import_is_external() {
    let files = [
        ("Cargo.toml", "[package]\nname = \"d\"\n"),
        ("src/lib.rs", "use dep::run;\npub fn f() { run(); }\n"),
    ];
    let g = build_from(&files).graph;
    let e = find(&g, EdgeKind::Calls, "d::f", "? run").expect("call kept");
    assert!(
        e.limitations.iter().any(|r| r.as_str() == "external_crate"),
        "{:?}",
        e.limitations
    );
}

#[test]
fn cargo_auto_discovery_switches_are_honoured() {
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"a\"\nautobins = false\nautotests = false\nbuild = \"config/setup.rs\"\n\n[[bin]]\nname = \"kept\"\npath = \"src/kept.rs\"\n",
        ),
        ("src/main.rs", "fn main() {}\n"),
        ("src/bin/helper.rs", "fn main() {}\n"),
        ("src/kept.rs", "fn main() {}\n"),
        ("tests/helper.rs", "fn h() {}\n"),
        ("build.rs", "fn main() {}\n"),
        ("config/setup.rs", "fn main() {}\n"),
    ];
    let g = build_from(&files).graph;
    let roots: Vec<(&str, &str)> = g
        .crates
        .iter()
        .map(|c| (c.target.as_str(), c.root.as_str()))
        .collect();
    assert!(roots.contains(&("bin", "src/kept.rs")), "{roots:?}");
    assert!(roots.contains(&("build", "config/setup.rs")), "{roots:?}");
    for gone in [
        "src/main.rs",
        "src/bin/helper.rs",
        "tests/helper.rs",
        "build.rs",
    ] {
        assert!(
            !roots.iter().any(|(_, r)| *r == gone),
            "{gone} in {roots:?}"
        );
    }
    let no_build = [
        ("Cargo.toml", "[package]\nname = \"n\"\nbuild = false\n"),
        ("build.rs", "fn main() {}\n"),
    ];
    assert!(
        build_from(&no_build)
            .graph
            .crates
            .iter()
            .all(|c| c.target != "build")
    );
}

#[test]
fn a_lookup_cut_by_the_depth_limit_is_not_cached_for_shorter_routes() {
    // `lib` glob-imports a0 first (a 16-hop chain ending at a15) and then a15 directly;
    // a15 re-exports `leaf::target`. The direct route must still find it.
    let mut files: Vec<(String, String)> =
        vec![("Cargo.toml".into(), "[package]\nname = \"deep\"\n".into())];
    let mut lib = String::from("mod leaf;\n");
    for i in 0..16 {
        lib += &format!("mod a{i};\n");
    }
    lib += "use crate::a0::*;\nuse crate::a15::*;\npub fn f() { target(); }\n";
    files.push(("src/lib.rs".into(), lib));
    files.push(("src/leaf.rs".into(), "pub fn target() {}\n".into()));
    for i in 0..15 {
        files.push((
            format!("src/a{i}.rs"),
            format!("pub use crate::a{}::*;\n", i + 1),
        ));
    }
    files.push(("src/a15.rs".into(), "pub use crate::leaf::target;\n".into()));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let g = build_from(&refs).graph;
    // The a0 chain was cut, so the answer is a candidate, not settled; but it's there.
    assert_eq!(
        resolved(&g, EdgeKind::Calls, "deep::f"),
        ["one of [deep::leaf::target]"]
    );
}

#[test]
fn glob_graphs_deeper_than_the_use_limit_still_resolve_in_bounded_time() {
    // More mutually glob-importing modules than the 16-hop `use` limit, so lookups do hit
    // the cut; that must not make resolution exponential.
    let mut files = vec![(
        "Cargo.toml".to_string(),
        "[package]\nname = \"wide\"\n".to_string(),
    )];
    let mods: Vec<String> = (0..24).map(|i| format!("m{i}")).collect();
    files.push((
        "src/lib.rs".into(),
        mods.iter()
            .map(|m| format!("mod {m};\n"))
            .collect::<String>()
            + "pub fn f() { missing(); }\n",
    ));
    for m in &mods {
        let globs: String = mods
            .iter()
            .filter(|o| *o != m)
            .map(|o| format!("pub use crate::{o}::*;\n"))
            .collect();
        files.push((format!("src/{m}.rs"), globs + "pub fn g() { missing(); }\n"));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let started = std::time::Instant::now();
    let g = build_from(&refs).graph;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(find(&g, EdgeKind::Calls, "wide::f", "? missing").is_some());
}

#[test]
fn layered_glob_imports_dont_multiply_results() {
    // Eight layers of nine modules, each glob-importing every module of the next layer,
    // the last re-exporting one function: 9^8 routes to the same declaration. Results
    // must stay deduplicated so neither time nor memory multiplies.
    let (layers, width) = (8, 9);
    let mut files = vec![(
        "Cargo.toml".to_string(),
        "[package]\nname = \"lay\"\n".to_string(),
    )];
    let mut lib = String::from("mod leaf;\n");
    for l in 0..layers {
        for w in 0..width {
            lib += &format!("mod l{l}w{w};\n");
        }
    }
    for w in 0..width {
        lib += &format!("use crate::l0w{w}::*;\n");
    }
    lib += "pub fn f() { target(); }\n";
    files.push(("src/lib.rs".into(), lib));
    files.push(("src/leaf.rs".into(), "pub fn target() {}\n".into()));
    for l in 0..layers {
        for w in 0..width {
            let body = if l + 1 == layers {
                "pub use crate::leaf::target;\n".to_string()
            } else {
                (0..width)
                    .map(|n| format!("pub use crate::l{}w{n}::*;\n", l + 1))
                    .collect()
            };
            files.push((format!("src/l{l}w{w}.rs"), body));
        }
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let started = std::time::Instant::now();
    let g = build_from(&refs).graph;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(
        find(&g, EdgeKind::Calls, "lay::f", "lay::leaf::target").is_some(),
        "{:?}",
        resolved(&g, EdgeKind::Calls, "lay::f")
    );
}

#[test]
fn a_lookup_cut_short_by_its_budget_is_not_settled() {
    // `first` and `last` both define `hit`, with enough empty glob-imported modules
    // between them to spend the lookup budget: finding `first::hit` alone mustn't read
    // as the one answer, and calls coverage mustn't stay complete.
    const N: usize = 16_000;
    let mut lib = String::from("mod first { pub fn hit() {} }\n");
    for i in 0..N {
        lib += &format!("mod m{i} {{}}\n");
    }
    lib += "mod last { pub fn hit() {} }\nuse first::*;\n";
    for i in 0..N {
        lib += &format!("use m{i}::*;\n");
    }
    lib += "use last::*;\npub fn f() { hit(); }\n";
    let g = build_from(&[
        ("Cargo.toml", "[package]\nname = \"cut\"\n"),
        ("src/lib.rs", &lib),
    ])
    .graph;
    let call = edges_of(&g, EdgeKind::Calls)
        .into_iter()
        .find(|e| endpoint(&g, &e.from) == "cut::f")
        .expect("the call is recorded");
    assert!(!matches!(call.to, Target::Resolved(_)), "{:?}", call.to);
    assert!(
        call.limitations
            .iter()
            .any(|r| r.as_str() == "resolution_limit")
    );
    assert!(!g.coverage[&EdgeKind::Calls].is_complete());

    // Cut short with nothing found yet: unknown, not missing, and still not complete.
    let lib = lib.replace("pub fn hit() {}", "");
    let g = build_from(&[
        ("Cargo.toml", "[package]\nname = \"cut\"\n"),
        ("src/lib.rs", &lib),
    ])
    .graph;
    assert_eq!(resolved(&g, EdgeKind::Calls, "cut::f"), ["? hit"]);
    assert!(!g.coverage[&EdgeKind::Calls].is_complete());
}

#[test]
fn an_impl_whose_type_lookup_was_cut_short_is_not_complete() {
    // `S` is found through `leaf`, but the glob chain through a0 is longer than the 16-hop limit:
    // the `Implements` edge starts at `S`, says it was cut, and coverage says so.
    let mut files: Vec<(String, String)> =
        vec![("Cargo.toml".into(), "[package]\nname = \"imp\"\n".into())];
    let mut lib = String::from("mod leaf;\n");
    for i in 0..20 {
        lib += &format!("mod a{i};\n");
    }
    lib += "use crate::a0::*;\nuse crate::leaf::*;\npub trait T {}\nimpl T for S {}\n";
    files.push(("src/lib.rs".into(), lib));
    files.push(("src/leaf.rs".into(), "pub struct S;\n".into()));
    for i in 0..19 {
        files.push((
            format!("src/a{i}.rs"),
            format!("pub use crate::a{}::*;\n", i + 1),
        ));
    }
    files.push(("src/a19.rs".into(), String::new()));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let g = build_from(&refs).graph;
    let imp = edges_of(&g, EdgeKind::Implements);
    assert_eq!(imp.len(), 1);
    assert_eq!(endpoint(&g, &imp[0].from), "imp::leaf::S");
    assert!(
        imp[0]
            .limitations
            .iter()
            .any(|r| r.as_str() == "resolution_limit"),
        "{:?}",
        imp[0].limitations
    );
    assert_eq!(imp[0].confidence, GraphConfidence::Low);
    assert!(!g.coverage[&EdgeKind::Implements].is_complete());
}
