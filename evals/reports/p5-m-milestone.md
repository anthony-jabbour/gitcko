# Phase 5 Milestone M — close-out

**Date:** 2026-11-16 UTC  
**Tip:** `f5a92ab` (B4 MCP on main; B1–B6 merged)  
**Live spend:** none (cassette path; OPENROUTER key present in `.env` but unused)

## B7 cut

Cut to **Phase 6** (no implementation PR). Reason: overlay wrong-bytes risk (P5_PLAN §9 early-fail / Q10).  
`--staged` / `--worktree` stay deferred (clap unexpected-arg exit 2 today; exit 6 when known-unimplemented).  
Full text for Docs D: `evals/reports/p5-m-b7-cut.md` (also summarized in `p5-m-mvp-4.1.md`).

## Checks

| Item | Result | Evidence |
|---|---|---|
| Install matrix on main | **green** | [run 43789279891](https://github.com/anthony-jabbour/gitcko/actions/runs/43789279891) (workflow_dispatch) |
| Last B6 PR install | green | run 43784299387 on `p5/6-release` |
| `release.yml` present | yes | tag trigger `v*`; needs human `release` env |
| Signed pre-release tag | **blocked** | `gh api …/environments` → `total_count: 0`; needs Anthony: `minisign -G`, pin pubkey, create `release` env + reviewer + `MINISIGN_SECRET_KEY` |
| 60s fuzz × 8 targets | **clean** | `evals/reports/p5-m-fuzz-60s.log` |
| Cassette MVP path | **pass** | `evals/reports/p5-m-eval-smoke.log` — golden 108 ok; Suite P/F 100% honest; every gate passed |
| MVP §4.1 via binary | see table | `evals/reports/p5-m-mvp-4.1.md` |

## Fuzz (60s each)

| Target | Runs (~61s) |
|---|---|
| config_load | 356 810 |
| diff_parse | 3 328 848 |
| mcp_args | 163 278 |
| model_output | 1 467 827 |
| provider_stream | 484 031 |
| syntax_extract | 129 116 |
| tool_args | 238 856 |
| user_config | 896 197 |

No crashes / artifacts reported.

## MVP §4.1

| Gate | Result |
|---|---|
| Rust workspace compiles | PASS |
| CLI path (`gitcko version`) | PASS |
| safe repo snapshot/diff (committed) | PASS |
| ≥1 language adapter (Rust) | PASS |
| basic graph/chunks/search (`gitcko index`) | PASS |
| one model provider (`setup` ollama) | PASS |
| Base review pipeline | PASS (cassette `eval --smoke`; CLI reaches snapshot without live) |
| structured finding output | PASS (`--json` / goldens) |
| deterministic fixtures | PASS |
| `--staged`/`--worktree` | DEFERRED (B7→P6) |

## End-of-phase checklist (P5_PROMPT done-condition)

| Criterion | Status |
|---|---|
| B1–B6 merged; B7 cut | yes |
| Main CI | B6 green; B4 push CI in flight at report time |
| MVP §4.1 on Rust through CLI path | yes (binary + cassettes) |
| Signed release builds + install on 3 OS | install matrix green; **signed tag blocked on Anthony** |
| Incomplete ≠ clean (CLI/TUI/MCP) | covered by B2/B4/B5 tests + Suite F cassettes |
| 60s fuzz per target | clean |
| Decisions log / Changelog / Features / RM-1 | **Docs D** (next) |
| `work_state` | none present in this session; checklist above substitutes |

## Next

1. Anthony: create `release` environment + minisign key; tag a pre-release when ready.  
2. Docs D on the private planning repo (decisions incl. B7 cut, spec edits, Notion, RM-1).  
3. Phase 6 proposal after D.
