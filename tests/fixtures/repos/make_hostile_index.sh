#!/usr/bin/env bash
# A Rust repository armed every way a build or package tool would run code: a build
# script, a proc-macro crate, a cargo runner, an npm postinstall and a Makefile, plus
# git hooks, a filter and an fsmonitor. Each would create ../PWNED. Indexing and
# querying this repo must never create it (INDEXING §34, SECURITY §14). It also holds
# files built to hit limits: a 5 MB single line, 10,000-deep nesting, and a binary
# named `.rs`. The git config is written after the commit, so building it fires nothing.
set -euo pipefail

parent="$(cd .. && pwd)"
pwn="touch \"$parent/PWNED\""

git init -q -b main
mkdir -p src macros/src .cargo
printf '[package]\nname = "armed"\nbuild = "build.rs"\n\n[dependencies]\nmacros = { path = "macros" }\n' > Cargo.toml
printf 'fn main() {\n    std::process::Command::new("sh").arg("-c").arg(r#"%s"#).status().unwrap();\n}\n' "$pwn" > build.rs
printf 'pub fn armed() -> u32 {\n    macros::boom!();\n    1\n}\n\nmod deep;\nmod long;\nmod disguised;\n' > src/lib.rs
printf '[package]\nname = "macros"\n\n[lib]\nproc-macro = true\n' > macros/Cargo.toml
printf 'use proc_macro::TokenStream;\n#[proc_macro]\npub fn boom(_: TokenStream) -> TokenStream {\n    std::process::Command::new("sh").arg("-c").arg(r#"%s"#).status().unwrap();\n    TokenStream::new()\n}\n' "$pwn" > macros/src/lib.rs
printf '[target.%s]\nrunner = "sh -c %s"\n' "'cfg(all())'" "'$pwn'" > .cargo/config.toml
printf '{"name":"armed","scripts":{"postinstall":"%s","prepare":"%s"}}\n' "touch ../PWNED" "touch ../PWNED" > package.json
printf 'all:\n\t%s\n' "$pwn" > Makefile
# Limits: a 5 MB line, nesting deeper than any parser walk, NUL bytes behind `.rs`.
head -c 5000000 /dev/zero | tr '\0' 'x' > src/long.rs
{ printf 'pub fn deep() {\n'; for _ in $(seq 10000); do printf '{'; done; for _ in $(seq 10000); do printf '}'; done; printf '\n}\n'; } > src/deep.rs
printf 'fn x() {}\n\0\0\0\0binary\0' > src/disguised.rs
git add -A
git commit -q -m armed

evil="$parent/evil.sh"
printf '#!/bin/sh\ntouch "%s/PWNED"\ncat "$1" 2>/dev/null || cat\n' "$parent" > "$evil"
chmod +x "$evil"
mkdir -p .git/info
printf '* filter=evil diff=evil\n' > .git/info/attributes
git config filter.evil.clean "sh '$evil'"
git config filter.evil.smudge "sh '$evil'"
git config filter.evil.required true
git config diff.evil.textconv "sh '$evil'"
git config core.fsmonitor "sh '$evil'"
mkdir -p .git/hooks
for hook in pre-commit post-checkout post-merge post-rewrite reference-transaction pre-auto-gc; do
    printf '#!/bin/sh\nsh "%s"\n' "$evil" > ".git/hooks/$hook"
    chmod +x ".git/hooks/$hook"
done
