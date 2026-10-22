# Diff parser fixtures

Each `.diff` file is unedited output of `git diff` (git 2.55) on a throwaway repository, so the parser is tested against what git really prints: copies and renames (`-C --find-copies-harder`), binary changes, mode changes, new, deleted and empty files, a symlink, `\ No newline at end of file`, a path with a space (git adds a trailing tab) and a non-ASCII path (git quotes it). `git_rename_symlink_mode.diff` also adds the first fixture as a file, so whole diff headers appear as `+` lines inside a hunk, which the parser must treat as content.

`git_format_patch.diff` is unedited default `git format-patch` output: mail headers and a diffstat before the diff, and a `-- ` signature with the git version after it.

Cases that can't be committed as files are built inside the tests: CRLF content (`.gitattributes` would turn it into LF) and a 50,000-line hunk.

These are parser fixtures. Annotated review fixtures (a diff plus the findings Gitcko should report) belong in `tests/fixtures/diffs/` at the workspace root.
