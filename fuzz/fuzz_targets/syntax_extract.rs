// SPDX-License-Identifier: AGPL-3.0-or-later

//! Any text as a Rust file: extraction must never panic or hang, every span must lie
//! inside the text, every index must point inside its list, and the same text must give
//! the same result twice (TESTING §10, "tree-sitter extraction"; TM-38).

#![no_main]

use std::time::Duration;

use gitcko_syntax::{Language, ParseLimits, adapter};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    // Tighter than the defaults so one input stays fast; the limits themselves are part
    // of what's tested.
    let limits = ParseLimits {
        timeout: Duration::from_millis(500),
        max_nodes: 200_000,
        max_depth: 256,
        max_symbols: 2_000,
        max_references: 10_000,
    };
    let rust = adapter(Language::Rust).expect("the Rust adapter is built in");
    let x = rust.extract(source, &limits);
    let len = source.len();
    let spans = x
        .symbols
        .iter()
        .flat_map(|s| [s.span, s.name_span])
        .chain(x.imports.iter().map(|i| i.span))
        .chain(x.references.iter().map(|r| r.span))
        .chain(x.implementations.iter().map(|i| i.span))
        .chain(x.extensions.iter().map(|e| e.span))
        .chain(x.syntax_errors.iter().copied());
    for span in spans {
        assert!(span.start_byte() <= span.end_byte() && span.end_byte() <= len);
    }
    for (i, s) in x.symbols.iter().enumerate() {
        assert!(s.parent.is_none_or(|p| p < i));
        assert!(s.signature.as_ref().is_none_or(|g| g.chars().count() <= 200));
    }
    assert!(x.references.iter().all(|r| r.enclosing.is_none_or(|e| e < x.symbols.len())));
    assert!(x.modules.iter().all(|m| m.symbol < x.symbols.len()));
    assert!(x.extensions.iter().all(|e| e.symbol < x.symbols.len()));
    // Scopes point only backwards, so rebuilding a path always ends.
    for (i, scope) in x.scopes.iter().enumerate() {
        assert!(scope.parent.is_none_or(|p| p < i));
    }
    let scopes = x
        .symbols
        .iter()
        .map(|s| s.scope)
        .chain(x.imports.iter().map(|i| i.scope))
        .chain(x.references.iter().map(|r| r.scope));
    for scope in scopes {
        assert!(scope.is_none_or(|s| s < x.scopes.len()));
    }
    assert!(x.symbols.len() <= limits.max_symbols);
    assert!(x.references.len() <= limits.max_references);
    // A timeout makes the result depend on speed, so compare only finished parses.
    if x.coverage.is_complete() {
        assert_eq!(rust.extract(source, &limits), x);
    }
});
