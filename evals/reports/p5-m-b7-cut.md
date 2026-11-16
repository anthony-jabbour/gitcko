# Phase 5 — B7 cut (for Docs D decisions log)

**Date:** 2026-11-16 (UTC)  
**Status:** Cut — do not implement in P5.

## Decision

Cut Phase 5 batch **B7 (staged/working-copy snapshot overlay)** to **Phase 6**.

## Authority

- P5_PLAN §9 early-fail: “Cut B7 to P6 rather than ship a snapshot that could read the wrong bytes”
- Q10 (approved as cuttable last batch)

## Reason

**Overlay wrong-bytes risk.** The engine and tools read git objects of commits; an in-memory path→blob overlay that can disagree with committed bytes is the highest-risk remaining P5 surface. Prefer cutting scope before the gate over shipping a snapshot path that could review the wrong content.

## What stays in P5

- CLI/TUI `--staged` / `--worktree` remain **deferred** (not advertised in clap today → unexpected-arg exit 2; when advertised as known-unimplemented they exit **6**, per CLI_SPEC / P5_PLAN §5.1).
- No B7 implementation PR.

## Docs D should land

- One decisions-log row in `docs/research/DOC_REWRITE_TRACKER.md` (Owner decisions log) with the above.
- Spec edits: CLI_SPEC / TUI_SPEC / ROADMAP note staged/worktree → P6 with F-12-era snapshot work as appropriate.
