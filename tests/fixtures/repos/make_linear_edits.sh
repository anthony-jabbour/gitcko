#!/usr/bin/env bash
# Branches `base` and `main` (head) with one of each kind of change between them.
# Plumbing (`update-index`) sets the symlink, the case-only twin and the executable
# bit, so the result is identical on Windows, where the disk can't hold them.
set -euo pipefail

git init -q -b main
seq 1 20 > numbers.txt
printf 'alpha\nbeta\ngamma\ndelta\nepsilon\n' > moved.txt
printf 'same\ncontent\nhere\n' > exact.txt
printf '#!/bin/sh\necho hi\n' > run.sh
printf 'gone\n' > deleted.txt
printf '\000\001bin' > blob.bin
printf 'u\n' > 'ünïcode.txt'
printf 'x\n' > 'with space.txt'
git add -A
git commit -q -m base
git branch base

{ seq 1 4; echo five; seq 6 20; } > numbers.txt
mkdir sub
git mv moved.txt sub/moved.txt
printf 'alpha\nbeta\nGAMMA\ndelta\nepsilon\n' > sub/moved.txt
git mv exact.txt exact-renamed.txt
git rm -q deleted.txt
printf 'new\n' > added.txt
printf '\000\002bin2' > blob.bin
printf 'v\n' > 'ünïcode.txt'
printf 'y' > 'with space.txt'
git add -A
git update-index --chmod=+x run.sh
link=$(printf 'numbers.txt' | git hash-object -w --stdin)
git update-index --add --cacheinfo "120000,$link,link.txt"
twin=$(printf 'twin\n' | git hash-object -w --stdin)
git update-index --add --cacheinfo "100644,$twin,README.md" --cacheinfo "100644,$twin,readme.md"
git commit -q -m head
