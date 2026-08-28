# 0033 — Remove the unreachable Bloom-filter join-probe path

**Status:** Accepted · **Date:** 2026-08-28

## Context

#464 (spec 011 Requirement 6) added a Bloom-filter pre-check for a join
level whose single `ON` equality had no structural rowid/unique-index
seek: `choose_bloom_probe` gated on `ANALYZE` stats recording at least
`MIN_ROWS_TO_BLOOM` (25) rows, and a hit compiled a one-time `FilterAdd`
pre-pass plus a per-outer-row `Filter` check ahead of the level's
`Rewind`/`Next` scan.

#545 later added a strictly stronger alternative: a transient automatic
index (`OpenEphemeral` + `AutoIndexInsert`/`AutoIndexSeek`) for the same
shape of join level, gated on the identical `MIN_ROWS_TO_AUTO_INDEX`
threshold (also 25) via `choose_auto_index_probe`, and tried first in
`compile_join_level_traverse`. Since both functions share the same row
threshold and the same equality/safety-of-probe conditions
(`top_level_equality_operands`, `expr_is_safe_join_probe`,
`column_index`), any input that would satisfy the Bloom probe's
conditions satisfies the auto-index probe's conditions first — the
Bloom branch has compiled into zero real programs since #545 landed.
Spec 011's own Requirement 6 text already documented this as "not
reachable today," but nothing had revisited the code itself (#623).

## Decision

**Delete the Bloom-filter join-probe path entirely** rather than keep
it as a defensive fallback or engineer a genuine divergence between the
two thresholds:

- `choose_bloom_probe`/`BloomProbe`/`MIN_ROWS_TO_BLOOM`
  (`src/codegen/select/join_access.rs`) and their unit tests.
- The `bloom_probe` gating and emission blocks in
  `compile_join_level_traverse` (`src/codegen/select/joins/level.rs`).
- `Opcode::FilterAdd`/`Opcode::Filter`, their dispatch arms
  (`src/vdbe/exec.rs`), `explain`/EXPLAIN formatting
  (`src/vdbe/explain.rs`), and the `Vm::filters` slot table plus
  `filter_add`/`filter_might_contain` methods.
- `src/vdbe/filter.rs` (`BloomFilterState`) in full.
- Spec 011 Requirement 6 and its two scenarios.

## Alternatives rejected

- **Keep it as a defensive fallback (status quo).** This is what spec
  011 already chose once; revisiting it, permanently-dead code with no
  test exercising it is a liability (it can silently rot, and a reader
  has to independently re-derive that it's dead every time they touch
  this area) with no offsetting benefit — nothing consumes it.
- **Make the two thresholds/conditions genuinely diverge**, so the
  Bloom path becomes a real second strategy for cases the auto-index
  doesn't cover. Rejected: no concrete case motivates a divergence today
  (auto-index's cost profile — one scan to build, O(1) seeks after — is
  strictly better than a Bloom pre-check's own scan-then-maybe-skip
  whenever both are legal), and inventing one speculatively contradicts
  this repo's simplicity-first convention. If a real motivating case
  arises later, the right move is to re-add a Bloom (or better) probe
  with a test proving it actually fires — not to keep an already-dead
  implementation on the chance one might.

## Consequences

- `Opcode` sheds two variants that were already excluded from
  `Opcode::ALL` (they postdated the V2 oracle harvest), so opcode
  completeness accounting (`tools/opcodes-v2.json`, 68/68) is
  unaffected.
- Spec 011 drops from 6 to 5 requirements; traceability/assurance
  numbers move accordingly (82 requirements / 264 scenarios, both still
  100% test-backed).
- Any future join-level pre-check optimization starts from a clean
  slate rather than resurrecting or diverging from this implementation.
