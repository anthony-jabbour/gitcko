// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Rust adapter on synthetic fixtures (INDEXING §38.2).

use std::time::Duration;

use gitcko_domain::{Coverage, SourceSpan};
use gitcko_syntax::{Extraction, Language, ParseLimits, adapter};
use gitcko_test_support::workspace_root;
use proptest::prelude::*;

fn extract(source: &str, limits: &ParseLimits) -> Extraction {
    adapter(Language::Rust).unwrap().extract(source, limits)
}

fn fixture(name: &str) -> String {
    let path = workspace_root().join("tests/fixtures/lang/rust").join(name);
    // Normalised so a Windows checkout with CRLF gives the same snapshot.
    std::fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

fn lines(span: &SourceSpan) -> String {
    let (a, b) = span.lines();
    if a == b {
        format!("L{a}")
    } else {
        format!("L{a}-{b}")
    }
}

/// A compact, stable rendering: one line per fact, no byte offsets.
fn render(x: &Extraction) -> String {
    let mut out = format!(
        "coverage: {:?}\nreference_coverage: {:?}\n",
        x.coverage, x.reference_coverage
    );
    for (i, s) in x.symbols.iter().enumerate() {
        out += &format!(
            "sym#{i} {:?}({}) {} scope=[{}]{}{} {:?}{}{} {}{}\n",
            s.kind,
            s.native_kind,
            s.name,
            x.scope_path(s.scope).join("::"),
            s.discriminator
                .as_deref()
                .map(|d| format!(" disc={d}"))
                .unwrap_or_default(),
            s.parent
                .map(|p| format!(" parent=#{p}"))
                .unwrap_or_default(),
            s.visibility,
            if s.in_test { " test" } else { "" },
            if s.near_syntax_error {
                " near_error"
            } else {
                ""
            },
            lines(&s.span),
            s.signature
                .as_deref()
                .map(|g| format!(" sig=`{g}`"))
                .unwrap_or_default(),
        );
    }
    for i in &x.imports {
        out += &format!(
            "use {}{}{}{} scope=[{}] {}\n",
            i.path.join("::"),
            if i.glob { "::*" } else { "" },
            i.alias
                .as_deref()
                .map(|a| format!(" as {a}"))
                .unwrap_or_default(),
            if i.public { " pub" } else { "" },
            x.scope_path(i.scope).join("::"),
            lines(&i.span),
        );
    }
    for r in &x.references {
        out += &format!(
            "ref {:?} {}{} {}\n",
            r.kind,
            r.path.join("::"),
            r.enclosing.map(|e| format!(" in=#{e}")).unwrap_or_default(),
            lines(&r.span),
        );
    }
    for m in &x.modules {
        out += &format!(
            "mod {} path={:?} scope=[{}] sym=#{}\n",
            m.name,
            m.path_attr,
            x.scope_path(m.scope).join("::"),
            m.symbol
        );
    }
    for m in &x.implementations {
        out += &format!(
            "impl {} for {} {}\n",
            m.trait_path
                .as_ref()
                .map(|t| t.join("::"))
                .unwrap_or("-".into()),
            m.type_path.join("::"),
            lines(&m.span)
        );
    }
    for e in &x.extensions {
        out += &format!(
            "extends #{} {} {}\n",
            e.symbol,
            e.target.join("::"),
            lines(&e.span)
        );
    }
    for e in &x.syntax_errors {
        out += &format!("syntax_error {}\n", lines(e));
    }
    out
}

#[test]
fn declarations() {
    insta::assert_snapshot!(render(&extract(
        &fixture("items.rs"),
        &ParseLimits::default()
    )));
}

#[test]
fn use_trees_are_flattened() {
    insta::assert_snapshot!(render(&extract(
        &fixture("uses.rs"),
        &ParseLimits::default()
    )));
}

#[test]
fn calls_and_references() {
    insta::assert_snapshot!(render(&extract(
        &fixture("calls.rs"),
        &ParseLimits::default()
    )));
}

#[test]
fn tests_and_test_modules() {
    insta::assert_snapshot!(render(&extract(
        &fixture("tests.rs"),
        &ParseLimits::default()
    )));
}

#[test]
fn syntax_errors_keep_what_can_be_read() {
    let x = extract(&fixture("broken.rs"), &ParseLimits::default());
    assert!(
        matches!(&x.coverage, Coverage::Partial { reason, .. } if reason.as_str() == "syntax_errors")
    );
    let names: Vec<&str> = x.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"fine") && names.contains(&"AfterTheError"),
        "{names:?}"
    );
    insta::assert_snapshot!(render(&x));
}

#[test]
fn methods_of_two_impls_with_the_same_name_stay_distinct() {
    // `<Point as Display>::fmt` and `<Point as Debug>::fmt` (INDEXING §38.3).
    let x = extract(&fixture("items.rs"), &ParseLimits::default());
    let fmts: Vec<_> = x
        .symbols
        .iter()
        .filter(|s| s.name == "fmt")
        .map(|s| (x.scope_path(s.scope), s.discriminator.clone()))
        .collect();
    assert_eq!(fmts.len(), 2);
    assert_ne!(fmts[0], fmts[1]);
}

#[test]
fn deep_nesting_hits_the_depth_limit_instead_of_the_stack() {
    let source = format!("fn f() {{ {} }}", "{".repeat(10_000) + &"}".repeat(10_000));
    let x = extract(&source, &ParseLimits::default());
    assert!(
        matches!(&x.coverage, Coverage::Partial { reason, .. } if reason.as_str() == "depth_limit"),
        "{:?}",
        x.coverage
    );
    assert_eq!(x.symbols[0].name, "f");
}

#[test]
fn a_parse_over_its_time_limit_is_unavailable_not_empty() {
    let limits = ParseLimits {
        timeout: Duration::ZERO,
        ..ParseLimits::default()
    };
    let source = "fn f() {}\n".repeat(20_000);
    let x = extract(&source, &limits);
    assert!(x.symbols.is_empty());
    assert!(
        matches!(&x.coverage, Coverage::Unavailable { reason } if reason.as_str() == "parse_timeout"),
        "{:?}",
        x.coverage
    );
    assert!(!x.reference_coverage.is_complete());
}

#[test]
fn node_and_symbol_limits_cut_with_a_reason() {
    let source = "fn a() {}\nfn b() {}\nfn c() {}\n";
    let x = extract(
        source,
        &ParseLimits {
            max_symbols: 2,
            ..ParseLimits::default()
        },
    );
    assert_eq!(x.symbols.len(), 2);
    assert!(
        matches!(&x.coverage, Coverage::Partial { reason, omitted: Some(1) } if reason.as_str() == "symbol_limit"),
        "{:?}",
        x.coverage
    );

    let x = extract(
        source,
        &ParseLimits {
            max_nodes: 5,
            ..ParseLimits::default()
        },
    );
    assert!(
        matches!(&x.coverage, Coverage::Partial { reason, .. } if reason.as_str() == "node_limit"),
        "{:?}",
        x.coverage
    );

    let x = extract(
        "fn a() { b(); c(); d(); }",
        &ParseLimits {
            max_references: 1,
            ..ParseLimits::default()
        },
    );
    assert_eq!(x.references.len(), 1);
    assert!(
        matches!(&x.reference_coverage, Coverage::Partial { reason, omitted: Some(2) } if reason.as_str() == "reference_limit"),
        "{:?}",
        x.reference_coverage
    );
}

#[test]
fn calls_inside_macro_arguments_are_guesses_and_say_so() {
    let x = extract(
        "fn t() { assert_eq!(add(1, 2), 3); }",
        &ParseLimits::default(),
    );
    assert!(
        x.references
            .iter()
            .any(|r| r.path == ["add"] && format!("{:?}", r.kind) == "MacroArgCall")
    );
    assert!(
        matches!(&x.reference_coverage, Coverage::Partial { reason, .. } if reason.as_str() == "macro_arguments")
    );
    // `.name(` inside the arguments is a method call on some value, not a free function.
    let x = extract("fn t() { assert!(v.is_empty()); }", &ParseLimits::default());
    let kinds: Vec<_> = x
        .references
        .iter()
        .map(|r| (r.path.join("::"), format!("{:?}", r.kind)))
        .collect();
    assert!(
        kinds.contains(&("is_empty".into(), "MethodCall".into())),
        "{kinds:?}"
    );
    // A file with no macro arguments keeps complete reference coverage.
    let x = extract("fn t() { add(1, 2); }", &ParseLimits::default());
    assert!(x.reference_coverage.is_complete());
}

#[test]
fn the_version_names_the_grammar() {
    let version = adapter(Language::Rust).unwrap().version();
    assert!(version.contains("tree-sitter-rust-0.24"), "{version}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Random edits to a real fixture never produce a span outside the text, a span
    /// that ends before it starts, or an index pointing past its list.
    #[test]
    fn spans_and_indexes_always_stay_in_bounds(
        at in 0usize..2_000,
        insert in "[{}()<>;:,.a-z \n\"'#!&|*/-]{0,40}",
    ) {
        let base = fixture("items.rs") + &fixture("calls.rs") + &fixture("tests.rs");
        let mut at = at.min(base.len());
        while !base.is_char_boundary(at) { at -= 1; }
        let source = format!("{}{}{}", &base[..at], insert, &base[at..]);
        let x = extract(&source, &ParseLimits::default());
        let len = source.len();
        let spans = x.symbols.iter().flat_map(|s| [s.span, s.name_span])
            .chain(x.imports.iter().map(|i| i.span))
            .chain(x.references.iter().map(|r| r.span))
            .chain(x.syntax_errors.iter().copied());
        for span in spans {
            prop_assert!(span.end_byte() <= len);
        }
        for (i, s) in x.symbols.iter().enumerate() {
            prop_assert!(s.parent.is_none_or(|p| p < i));
            prop_assert!(s.scope.is_none_or(|sc| sc < x.scopes.len()));
        }
        for (i, scope) in x.scopes.iter().enumerate() {
            prop_assert!(scope.parent.is_none_or(|p| p < i));
        }
        for r in &x.references {
            prop_assert!(r.enclosing.is_none_or(|e| e < x.symbols.len()));
        }
        // Same input, same output.
        prop_assert_eq!(&extract(&source, &ParseLimits::default()), &x);
    }
}

#[test]
fn a_symbol_overlapping_a_syntax_error_is_flagged() {
    let x = extract(
        "fn ok() {}\nfn hurt() { let x = ; }\n",
        &ParseLimits::default(),
    );
    let flags: Vec<_> = x
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.near_syntax_error))
        .collect();
    assert_eq!(flags, [("ok", false), ("hurt", true)]);
}

#[test]
fn huge_generic_lists_and_values_stay_cheap() {
    // Thousands of type parameters, each visible to every child node, and a megabyte
    // constant: both must finish fast and keep only bounded text (TM-38).
    let params: Vec<String> = (0..5_000).map(|i| format!("T{i}")).collect();
    let source = format!(
        "fn f<{}>() {{ {} }}\nconst BIG: &str = \"{}\";\n",
        params.join(", "),
        "g(); ".repeat(5_000),
        "x".repeat(1 << 20),
    );
    let started = std::time::Instant::now();
    let x = extract(&source, &ParseLimits::default());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let big = x.symbols.iter().find(|s| s.name == "BIG").unwrap();
    assert!(big.signature.as_ref().unwrap().chars().count() <= 200);
}

#[test]
fn a_walk_over_its_time_limit_is_partial() {
    // The parse is quick but the walk has a lot to visit; a tiny budget stops it.
    let source = "fn f() { g(); }\n".repeat(50_000);
    let limits = ParseLimits {
        timeout: Duration::from_millis(1),
        ..ParseLimits::default()
    };
    let x = extract(&source, &limits);
    let reason = match &x.coverage {
        Coverage::Partial { reason, .. } | Coverage::Unavailable { reason } => reason.as_str(),
        Coverage::Complete {} | Coverage::Unknown { .. } => "complete",
    };
    assert!(
        reason == "walk_timeout" || reason == "parse_timeout",
        "{reason}"
    );
}

#[test]
fn use_trees_obey_the_same_limits_as_everything_else() {
    // `use a::{b::{c::{…}}}` thousands deep, and a list wider than the reference budget.
    let deep = format!("use {}x{};", "a::{".repeat(3_000), "}".repeat(3_000));
    let x = extract(&deep, &ParseLimits::default());
    assert!(!x.coverage.is_complete() || !x.reference_coverage.is_complete());
    assert!(x.imports.iter().all(|i| i.path.len() <= 64));
    let names: Vec<String> = (0..50).map(|i| format!("n{i}")).collect();
    let wide = format!("use a::{{{}}};", names.join(", "));
    let x = extract(
        &wide,
        &ParseLimits {
            max_references: 10,
            ..ParseLimits::default()
        },
    );
    assert_eq!(x.imports.len(), 10);
    assert!(
        matches!(&x.reference_coverage, Coverage::Partial { reason, omitted: Some(40) } if reason.as_str() == "reference_limit"),
        "{:?}",
        x.reference_coverage
    );
}

#[test]
fn self_with_an_alias_imports_the_list_path() {
    let x = extract("use a::b::{self as renamed, c};", &ParseLimits::default());
    let imports: Vec<_> = x
        .imports
        .iter()
        .map(|i| (i.path.join("::"), i.alias.clone()))
        .collect();
    assert_eq!(
        imports,
        [
            ("a::b".to_string(), Some("renamed".to_string())),
            ("a::b::c".to_string(), None)
        ]
    );
}

#[test]
fn a_path_attribute_keeps_its_exact_value() {
    let source = "#[path = \"some dir/x\\u{2e}rs\"]\nmod a;\n#[path = r\"raw dir/y.rs\"]\nmod b;\n";
    let x = extract(source, &ParseLimits::default());
    let paths: Vec<_> = x.modules.iter().map(|m| m.path_attr.clone()).collect();
    assert_eq!(
        paths,
        [
            Some("some dir/x.rs".to_string()),
            Some("raw dir/y.rs".to_string())
        ]
    );
}

#[test]
fn methods_of_two_types_with_the_same_last_name_stay_distinct() {
    let x = extract(
        "impl a::Thing { fn run() {} }\nimpl b::Thing { fn run() {} }\n",
        &ParseLimits::default(),
    );
    let scopes: Vec<_> = x.symbols.iter().map(|s| x.scope_path(s.scope)).collect();
    assert_ne!(scopes[0], scopes[1], "{scopes:?}");
}

#[test]
fn an_unfinished_file_is_not_complete() {
    // Tree-sitter recovers these as whole items with an unnamed `MISSING "}"` token.
    for source in ["struct S { a: u32", "mod m { fn f() {} "] {
        let x = extract(source, &ParseLimits::default());
        assert!(!x.coverage.is_complete(), "{source}: {:?}", x.coverage);
        assert!(x.symbols[0].near_syntax_error, "{source}");
    }
}

#[test]
fn use_tree_nesting_counts_against_max_depth() {
    let x = extract(
        "use a::{b::{c::{d::{e::x}}}};",
        &ParseLimits {
            max_depth: 6,
            ..ParseLimits::default()
        },
    );
    assert!(
        matches!(&x.coverage, Coverage::Partial { reason, .. } if reason.as_str() == "depth_limit"),
        "{:?}",
        x.coverage
    );
}

#[test]
fn generics_in_scope_are_capped_in_total() {
    // Ten nested functions with 64 type parameters each: only the first 256 names are
    // remembered, so a parameter from the innermost level is treated as a type name.
    let mut source = String::new();
    for level in 0..10 {
        let params: Vec<String> = (0..64).map(|i| format!("T{level}x{i}")).collect();
        source += &format!("fn f{level}<{}>() {{\n", params.join(", "));
    }
    source += "let _: T0x0; let _: T9x0;\n";
    source += &"}\n".repeat(10);
    let x = extract(&source, &ParseLimits::default());
    let types: Vec<&str> = x
        .references
        .iter()
        .filter(|r| format!("{:?}", r.kind) == "Type")
        .map(|r| r.path[0].as_str())
        .collect();
    assert!(!types.contains(&"T0x0"), "{types:?}");
    assert!(types.contains(&"T9x0"), "{types:?}");
}
