# Gitcko

Gitcko is an open-source AI code reviewer. It reviews a change with an understanding of the whole repository, not just the diff, and reports a summary, a 0–5 confidence score and a small number of high-signal findings, each with a severity, the cause, evidence and a suggested fix.

It runs on your own machine with a local model (Ollama) or a model key you bring, from the command line, a terminal UI, or your coding agent over MCP. A self-hosted server and a hosted service, Gitcko Cloud, come later.

> **Status: pre-alpha.** There is nothing to install yet. This repository is being set up: build tooling, CI and contributor docs first, then the engine. The first public preview is planned for the end of 2026.

## Licence

| What | Licence |
|---|---|
| The core: engine, CLI, terminal UI, MCP server, self-host server, dashboard | [AGPL-3.0-or-later](LICENSE) |
| Integrations: JSON schemas and the crate they come from, agent plugins and skills, the GitHub Action, config import | [Apache-2.0](LICENSES/Apache-2.0.txt) |

What the AGPL means for you: using Gitcko locally, or self-hosting it unchanged, is free and carries no obligations. If you modify it **and** let other people use your modified version over a network, you must offer them your modified source code (AGPL §13).

"Gitcko" and the Gitcko logo are trademarks; see [TRADEMARKS.md](TRADEMARKS.md).

## Contributing

Start with [CONTRIBUTING.md](CONTRIBUTING.md). Contributors sign a Contributor License Agreement once, before their first PR is merged. Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).

Report security problems privately, as described in [SECURITY.md](SECURITY.md). Never in a public issue.
