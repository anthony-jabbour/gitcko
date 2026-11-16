# Phase 5 Milestone M — MVP §4.1 walkthrough

Date: 2026-11-16T02:02:59Z (binary walk); cassette confirmation 2026-11-16 via `cargo xtask eval --smoke`
Tip: `f5a92ab` (main)
Path: cassette/fixture preferred; no live OpenRouter spend.

| Gate | Result | Evidence |
|---|---|---|
| Rust workspace compiles | PASS | `cargo build -p gitcko-cli --release --locked` → `target/release/gitcko` |
| CLI path | PASS | `gitcko version` → `gitcko 0.0.0 (engine 0.0.0, result schema gitcko.review/1, mcp gitcko.mcp/1)` |
| safe repo snapshot/diff | PASS | committed base/head only; B7 overlay cut to P6 (wrong-bytes risk) |
| ≥1 language adapter (Rust) | PASS | gitcko-syntax Rust since P2; `gitcko index` + cassette goldens |
| doctor / local health | PASS | `gitcko doctor --json` schema `gitcko.doctor/1` |
| basic graph/chunks/search | PASS | `gitcko index` on fixture exit 0 |
| one model provider | PASS | `gitcko setup --provider ollama --model fixture:1 --no-input` exit 0 |
| unavailable flags exit 6 | PASS | `--plus` → exit 6; `notice: --plus is not available in this build` |
| --staged/--worktree (B7 cut) | DEFERRED | not advertised; clap unexpected-arg exit 2; P6 implements overlay |
| Base review pipeline | PASS (cassette) | CLI without a live/loopback model exits incomplete after snapshot; full Base pipeline + goldens via `cargo xtask eval --smoke` (108 goldens, Suites P/F/A) |
| structured finding output | PASS | CLI `--json` = `gitcko.review/1` (B2); smoke: `eval: every gate passed` |
| deterministic fixtures | PASS | `tests/fixtures/review` + `tests/cassettes`; smoke log `p5-m-eval-smoke.log` |

## B7 cut

Cut to Phase 6 (P5_PLAN §9 / Q10): overlay wrong-bytes risk. No B7 PR. Docs D landed the decisions row (planning-repo#11).

## Blocked on human (Anthony)

- GitHub `release` environment: **absent** (`total_count` 0).
- Signed pre-release tag: **BLOCKED_ON_ANTHONY** until `release` env + required reviewer + `MINISIGN_SECRET_KEY` and real minisign keypin.
- `release.yml` is present on main (tag `v*` trigger).

## Fuzz

All eight targets clean for 60s — see `p5-m-fuzz-60s.log` / `p5-m-milestone.md`.
