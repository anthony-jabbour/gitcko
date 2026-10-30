# Test fixtures

Tests in this repo are deterministic. They never call a live model or the network, and they never use real customer code: every fixture is synthetic or comes from a project whose licence allows it.

Each folder below is created by the first fixture that needs it.

| Folder | Holds |
|---|---|
| `repos/` | Small Git repositories for end-to-end tests, each built by a `make_<name>.sh` script that commits exactly what the test needs. The script is the source of truth. Tests build each repository fresh with fixed author, dates and config, so commit ids are the same on every OS; caching the output as archives (gitoxide's pattern) can come later if the suite gets slow. Scripts use plumbing (`git update-index`) for entries a Windows disk can't hold, such as symlinks and case-only twins. (Pattern from [gitoxide](https://github.com/GitoxideLabs/gitoxide).) |
| `lang/<language>/` | Source files per supported language, with the expected symbols and call graphs as golden files. |
| `diffs/` | Annotated review fixtures: a diff plus the findings Gitcko is expected to report (format below). |
| `webhooks/github/`, `webhooks/origin/` | Recorded webhook deliveries from code hosts, signed with throwaway development secrets. |
| `tool_args/` | Seeds for the `tool_args` fuzz target: one byte picking a review tool, then the argument JSON a model would send. |
| `outputs/` | Model replies the output parser must accept or refuse (truncated JSON, extra prose, wrong values, invented paths and lines), with the expected result listed in `gitcko-prompts/tests/outputs.rs`. |
| `windows/` | Path cases that break on Windows: backslashes, drive letters, UNC paths, CRLF line endings, reserved names like `CON`, case-only collisions and very long paths. |

## Annotating expected findings

Mark each expected finding in a fixture with a comment under the code it points at. The carets cover the exact span, followed by the severity, category and a short name (the style comes from [rust-analyzer](https://github.com/rust-lang/rust-analyzer)'s test fixtures):

```rust
let total = items.len() / count;
//                       ^^^^^ finding: P1 logic division-by-zero
```

A fixture with **no** annotations asserts that Gitcko reports **nothing** for it. Clean fixtures are how we catch false positives, so add them as readily as buggy ones.

## Rules

- Synthetic content only. No secrets (not even expired ones), no private code, nothing copied from other products.
- Keep each fixture as small as the behaviour it tests.
- Planning-doc names (`SECURITY.md`, `ARCHITECTURE.md` and so on) are allowed here, so a fixture repo can look like a real project; the leak guard exempts `tests/fixtures/`.
