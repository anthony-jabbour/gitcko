## What and why

<!-- The problem and the scope of this change. -->

Closes #<!-- issue --> · RFC: <!-- link, if this needs one -->

## Tests

<!-- Tests or fixtures added, or why none are needed. -->

## Security and data flow

<!-- Does this change what source code, credentials or data leave the machine, which providers are called, or what permissions are needed? "None" is a fine answer. -->

## Compatibility

<!-- Schema, config, CLI flag or exit-code changes, and migration notes. -->

## Changelog

<!-- One line for users, or "none". -->

## AI assistance

<!-- Which AI tools helped, and how. A human author owns every line. -->

## Checklist

- [ ] `cargo xtask check` passes
- [ ] `cargo xtask leak-guard` passes: no secrets, no private or hosted-service code
- [ ] Every new source file has its `SPDX-License-Identifier` header
- [ ] Docs updated where behaviour changed
