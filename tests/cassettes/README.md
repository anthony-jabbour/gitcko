# Model cassettes

A cassette is a recorded model response. Every test and every `cargo xtask` run replays cassettes instead of calling a model, so the suite is fast, free, offline and gives the same result every time.

`gitcko-cassette` records and replays them. This file is the contract it follows.

## How a call finds its cassette

Each model call is keyed by a SHA-256 hash of:

1. the **model binding**, for example `ollama:<model>` or `openrouter:<model>`;
2. the **role** the call plays in a review;
3. the **prompt template and its version**;
4. the **rendered request**: the exact messages and parameters sent.

Change any of them and the key changes, so an edited prompt can never silently replay an old answer.

Precisely, the key is the SHA-256 of the canonical JSON (object keys sorted at every level) of:

```json
{
  "v": 1,
  "binding": { "provider": "ollama", "model": "<exact model>", "reasoning_effort": null, "data_policy": null, "context": 131072 },
  "role": "review",
  "prompt": { "id": "review", "version": "1" },
  "request": { "messages": [], "tools": [], "response_format": null, "max_output": 256, "reasoning_effort": null, "temperature": 0.0 }
}
```

Left out on purpose: the endpoint and port, the credential and headers, the binding's name, the run id, the attempt number, timeouts, deadlines and timestamps. Moving a server or rotating a key replays the same cassette.

## File format

One file per call, `<key>.json`, pretty-printed with a trailing newline, fields in this order:

| Field | Holds |
|---|---|
| `v` | Format version, `1` |
| `key` | The key above; replay checks it matches the file name |
| `role`, `binding`, `prompt` | What the call was, for reviewers reading the diff |
| `events` | The response as the router saw it: `text_delta`, `tool_call`, `usage`, `finish`, in order. Provider response ids and timestamps are not kept |

Only a call that ended with `finish` is recorded; a failed call is passed through and not written. Replay plays the events back exactly, and honours cancellation like a live call. Recording the same call twice writes the same bytes.

## Rules

- **A missing cassette fails the test.** The error shows the key and the exact command to record it. It never falls back to a live call.
- **Recording is explicit**, through a `cargo xtask` command that names the suite and the model. Review the recorded files in the same PR, like any other change.
- **A prompt or routing change re-records its cassettes in the same PR.** Old keys stop matching, so stale cassettes can't hide the change.
- **Cassettes test plumbing, not quality.** Re-recording a cassette makes the test pass whatever the model said, so review quality is measured separately, with live models (see [CONTRIBUTING.md](../../CONTRIBUTING.md#required-checks)).
- **A recording that would leak isn't written.** Before writing, the file is scanned for the binding's key, `Authorization`, `Bearer`, credential-shaped strings and the absolute paths of the home, temp and current directories. If any appear, the call ends with an error and no file is saved.
- **Contents are fixture data only.** Recordings come from the repositories in [`tests/fixtures/`](../fixtures/README.md), never from real code, and must not contain secrets.
