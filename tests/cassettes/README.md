# Model cassettes

A cassette is a recorded model response. Every test and every `cargo xtask` run replays cassettes instead of calling a model, so the suite is fast, free, offline and gives the same result every time.

The code that records and replays them lands with the model layer. This file is the contract it follows.

## How a call finds its cassette

Each model call is keyed by a SHA-256 hash of:

1. the **model binding**, for example `ollama:<model>` or `openrouter:<model>`;
2. the **role** the call plays in a review;
3. the **prompt template and its version**;
4. the **rendered request**: the exact messages and parameters sent.

Change any of them and the key changes, so an edited prompt can never silently replay an old answer.

## Rules

- **A missing cassette fails the test.** The error shows the key and the exact command to record it. It never falls back to a live call.
- **Recording is explicit**, through a `cargo xtask` command that names the suite and the model. Review the recorded files in the same PR, like any other change.
- **A prompt or routing change re-records its cassettes in the same PR.** Old keys stop matching, so stale cassettes can't hide the change.
- **Cassettes test plumbing, not quality.** Re-recording a cassette makes the test pass whatever the model said, so review quality is measured separately, with live models (see [CONTRIBUTING.md](../../CONTRIBUTING.md#required-checks)).
- **Contents are fixture data only.** Recordings come from the repositories in [`tests/fixtures/`](../fixtures/README.md), never from real code, and must not contain secrets.
