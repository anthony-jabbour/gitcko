# Contributing to Gitcko

Thanks for helping. Gitcko is an AI code reviewer, so we hold our own changes to a high bar: every PR is tested, reviewed, and checked so private data and secrets never land in this public repository.

Gitcko is pre-alpha. The build tooling and CI are being set up first; the engine comes next. Some of what follows describes tooling that is landing right now.

## Quick start

You need [rustup](https://rustup.rs) and Git 2.40 or newer. macOS and Windows are the supported development platforms. Linux is fully tested in CI and builds the same way, but it isn't an officially supported development platform yet; a ready-made Linux dev container is planned.

```bash
git clone https://github.com/anthony-jabbour/gitcko && cd gitcko
cargo xtask setup      # pinned tools and git hooks
cargo xtask check      # format, lint, tests: the same as CI
```

Looking for a first task? Issues labelled `good first issue` or `help wanted` are ready to pick up. Leave a comment so nobody else starts on it, and open a draft PR as soon as you have something to show.

## What belongs here

| Welcome | Not accepted |
|---|---|
| Review engine, indexing, validation, scoring | Secrets, credentials or keys of any kind |
| CLI, terminal UI, MCP server, self-host server | Code from private or customer repositories |
| Model adapters, language support, code-host adapters | Code, prompts, docs or assets copied from other products, even when they are MIT-licensed |
| Rules, test fixtures, docs | Billing, metering or other hosted-service internals |

## Issues

Use the issue forms:

| Form | For |
|---|---|
| **Bug** | Wrong behaviour: version, OS, command, expected vs actual |
| **False positive / missed bug** | Gitcko flagged something harmless or missed a real problem. Include a minimal repro, the finding (or the missed bug) and your reasoning. We turn these into test cases |
| **Feature** | A problem you want solved (not a solution) |
| **RFC** | A design-level change (see below) |

**Security problems never go in an issue.** See [SECURITY.md](SECURITY.md).

Labels: `type/{bug,false-positive,missed-bug,feature,rfc,docs,chore}`, `area/{engine,index,lang-*,model,scm,cli,tui,mcp,server,verify,config,docs}`, `good first issue`, `help wanted`, `needs-repro`, `needs-rfc`, `blocked`. New issues get a label within 3 business days. A `needs-repro` issue with no reply for 30 days is closed.

## RFCs

Open an RFC issue **before** a PR that changes:

- a public schema or interface: review output format, API, MCP tool schemas, config keys, exit codes;
- review behaviour, severity, scoring or model-routing defaults;
- a security default or trust boundary;
- dependencies, by adding one with network, native or `unsafe` code.

The RFC covers the problem, the proposal, alternatives, migration and how it will be tested. After at least 7 days of discussion a maintainer marks it `accepted` or `declined`, with reasons. The PR then updates the docs it affects together with the code. Bug fixes, docs and tests can skip the RFC step.

## Branches, commits and pull requests

- Fork, then branch from `main`: `fix/…`, `feat/…`, `docs/…`, `lang/<name>`, `rfc/<n>-…`.
- Use [Conventional Commits](https://www.conventionalcommits.org): `fix(index): …`, `feat(cli): …`. Mark a breaking change with `!` after the type and explain it in a `BREAKING CHANGE:` footer.
- One logical change per PR. Rebase on `main`; no merge commits. PRs are squash-merged.
- Fill in the PR template: the problem and scope, linked issue or RFC, tests added (or why none), security and data-flow impact, compatibility notes, a changelog line, and any AI assistance used.

## Required checks

| Check | Command | Applies to |
|---|---|---|
| Format | `cargo fmt --check` | all PRs |
| Lint | `cargo clippy --all-targets -- -D warnings` | all PRs |
| Tests (macOS, Windows, Linux) | `cargo nextest run` | all PRs |
| Leak guard (secrets and private paths) | `cargo xtask leak-guard` | all PRs and pushes |
| Licence headers | `cargo xtask spdx` | all PRs |
| Dependency policy | `cargo deny check` | `Cargo.lock` changes |
| Crate layering | `cargo xtask layers` | `Cargo.toml` changes |
| Snapshots | `cargo insta test --check --unreferenced=reject` (review changes with `cargo insta review`) | all PRs |
| Schema compatibility | `cargo xtask schema-check` (regenerate with `cargo xtask schema-gen`) | changes to `crates/gitcko-protocol/` or `integrations/schemas/` |
| CLA | CLA Assistant | all PRs |

`cargo xtask check` runs the first three. Tests are deterministic: they use the fixtures described in [tests/fixtures/README.md](tests/fixtures/README.md) and replay recorded model responses as described in [tests/cassettes/README.md](tests/cassettes/README.md), never a live model or the network. More checks (golden review fixtures, eval smoke tests, fuzzing) are added as the code they test lands. A PR isn't accepted just because it compiles.

**Unsafe code** is forbidden across the workspace. A crate that truly needs it requires an RFC and maintainer approval.

**Dependencies** must use a permissive licence (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode, CC0, 0BSD, BSL-1.0) or MPL-2.0. GPL, AGPL, LGPL, SSPL, BUSL and other copyleft or source-available licences are rejected by `cargo deny`. State the licence and origin of any third-party code in your PR.

## Licensing and the CLA

The core is **AGPL-3.0-or-later**. Integrations (schemas and the crate they're generated from, agent plugins and skills, the GitHub Action, config import) are **Apache-2.0**. Each source file starts with an `SPDX-License-Identifier` line for its folder's licence.

Before your first PR is merged, you sign a **Contributor License Agreement** once, through the CLA Assistant check on the PR. There's an [individual](docs/cla/individual.md) and a [corporate](docs/cla/corporate.md) version. **Both are drafts awaiting legal review. Until the final text is published and the CLA check is switched on, pull requests from outside contributors can't be merged.** You keep the copyright in your contribution. You give the project the right to use, sublicense and relicense it; this is what lets the hosted Gitcko Cloud build on the core.

**Our promise:** what you contribute stays available in this repository under AGPL-3.0 or a more permissive licence.

## AI-assisted contributions

AI tools are welcome. In the PR template, say which tools helped (for example: "used Copilot for the first draft of `parse_hunk`, then wrote the edge-case tests myself").

- **You are responsible for every line you submit**, whoever or whatever typed it. Read it, run it, and be ready to explain it in review.
- A test you add must fail before your fix and pass after it, even if an AI wrote it.
- We close PRs that look like unchecked machine output, such as large changes with no tests or explanation, or that ignore review feedback, and say why.

## Review, merge and releases

- Every PR gets a maintainer review. Once Gitcko can review code, it reviews every PR too; its findings are advisory and maintainers decide.
- Approvals: 1 maintainer. Security-labelled PRs, runtime-verification changes and PRs that implement an RFC need 2 once the project has two maintainers; until then the lead maintainer approves them.
- First maintainer response within 5 business days.
- Pull requests from forks run CI without access to any secrets. Workflows use read-only permissions by default and pin third-party actions by commit.
- Releases follow semver. `CHANGELOG.md` is generated from Conventional Commits. Tags are signed, and release artifacts come with checksums, signatures and build provenance.
- Deprecations: a public flag, config key or schema field gets one minor release of warnings before it's removed.

## Governance

Maintainers, listed in [MAINTAINERS.md](MAINTAINERS.md), decide merges, RFCs and releases. The lead maintainer breaks ties. Contributors become maintainers by invitation, after sustained, high-quality reviews and contributions. Decisions and RFC outcomes are recorded in their issue.

## Code of conduct

Everyone taking part follows the [Contributor Covenant 2.1](CODE_OF_CONDUCT.md). Report conduct problems privately to the maintainers in [MAINTAINERS.md](MAINTAINERS.md). While the project has a single maintainer, a report about that maintainer can't yet go to anyone independent; we'll name an independent contact as soon as the project has one.
