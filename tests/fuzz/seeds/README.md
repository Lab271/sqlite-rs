# Fuzz seeds

Committed, hand-picked seed inputs for each `tests/fuzz/fuzz_targets/*.rs`
target — one directory per target, matching its binary name. Unlike
`tests/fuzz/corpus/` (the corpus libFuzzer grows at runtime, gitignored —
see `tests/fuzz/.gitignore`), these are checked into git so a crash found
once can't silently regress and so a fresh `make fuzz-smoke`/`make
fuzz-<target>` run always starts from a non-empty, structurally-relevant
corpus instead of pure random bytes.

## Convention

- Every `make fuzz-*` target and `make fuzz-smoke` invoke
  `cargo fuzz run ... <target> tests/fuzz/corpus/<target>
  tests/fuzz/seeds/<target>`: libFuzzer treats the **first** `CORPUS`
  argument as the primary, writable corpus (where newly discovered inputs
  get saved) and every argument after it as read-only extra input. Always
  keep `tests/fuzz/corpus/<target>` first — passing only the seeds
  directory makes libFuzzer write its generated corpus directly into it,
  which is exactly the accident that filled this directory with ~7,000
  generated files the first time `make fuzz-smoke` ran (#615) and had to
  be reverted.
- When a fuzz run finds a crash, minimize it (`cargo fuzz tmin`) and add
  the minimized reproducer here as a new seed file before fixing the bug,
  so the fix is provably covered and the regression can't come back
  unnoticed.
- Filenames are free-form and descriptive (no required extension, except
  `parse_select`'s inputs are UTF-8 SQL text and use `.sql` for
  readability) — libFuzzer reads every file in the directory regardless
  of name.
