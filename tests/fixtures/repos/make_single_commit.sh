#!/usr/bin/env bash
# One commit with one file: the smallest repo, used to check that fixture repos come
# out byte-identical (same commit id) on macOS, Windows and Linux.
set -euo pipefail

git init -q -b main
printf 'hello\n' > README.md
git add README.md
git commit -q -m "first"
