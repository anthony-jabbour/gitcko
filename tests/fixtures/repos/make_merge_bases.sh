#!/usr/bin/env bash
# Merge-base cases: `left`/`main` fork from `root`; `cc_a`/`cc_b` are a criss-cross
# with two merge bases; `lonely` shares no history with the rest.
set -euo pipefail

commit() { printf '%s\n' "$1" > "$1.txt"; git add "$1.txt"; git commit -q -m "$1"; }

git init -q -b main
commit root
git tag root
git checkout -q -b left
commit l1
git checkout -q main
commit m1

git checkout -q -b cc_a root
commit a1
git checkout -q -b cc_b root
commit b1
a1=$(git rev-parse cc_a)
b1=$(git rev-parse cc_b)
git checkout -q cc_a
git merge -q --no-ff --no-edit -m a2 "$b1"
git checkout -q cc_b
git merge -q --no-ff --no-edit -m b2 "$a1"

git checkout -q --orphan lonely
git rm -q -r -f .
commit lonely
git checkout -q main
