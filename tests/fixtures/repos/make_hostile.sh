#!/usr/bin/env bash
# A repository armed to run commands: hooks, a clean/smudge filter, a textconv diff
# driver and an fsmonitor, each of which creates ../PWNED. Reading this repo with
# gitcko-git must never create it (SECURITY §14.2–14.3). The config is written after the
# commits, so building the fixture doesn't fire it.
set -euo pipefail

git init -q -b main
printf 'a\n' > file.txt
printf '* filter=evil diff=evil\n' > .gitattributes
git add -A
git commit -q -m base
printf 'b\n' > file.txt
git add file.txt
git commit -q -m head

evil="$(cd .. && pwd)/evil.sh"
printf '#!/bin/sh\ntouch "%s/PWNED"\ncat "$1" 2>/dev/null || cat\n' "$(cd .. && pwd)" > "$evil"
chmod +x "$evil"
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
