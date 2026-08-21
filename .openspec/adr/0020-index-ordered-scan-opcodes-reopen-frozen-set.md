# 0020 — `IdxRewind`/`IdxLast`/`IdxNext`/`IdxPrev` reopen the frozen V2 opcode set

**Status:** Accepted · **Date:** 2026-08-21

## Context

#296 found `tests/performance/engine.rs`'s `order_by_limit` benchmark
(`SELECT id, n, x, f, s FROM bench_data ORDER BY x DESC LIMIT 100`)
running ~570x–23,000x slower than the pinned oracle even after #129's
top-K bounded sorter, because there's an index on `x` — the oracle
walks that index b-tree directly in the needed order and stops after
`LIMIT` rows, doing no full scan and no sort at all. Reproducing that
shape needs a way to walk a real index b-tree cursor sequentially
(forward or backward), which nothing in the existing opcode set
provides: `SeekIndexEq`/`IdxRowid` (#243) are a one-shot point lookup —
`SeekIndexEq` builds a fresh `IndexCursor` per call and only records
the matched row's trailing rowid, with no persisted traversal position
for a following `Next`-style advance.

`Rewind`/`Last`/`Next` already exist for table cursors, but a real
index-read cursor (`CursorSlot::IndexRead`, opened by `OpenRead` with
`P5` nonzero) is a distinct cursor-slot variant — extending
`Rewind`/`Next` to also handle it would work for the forward direction,
but there's no `Prev` at all (only `Last` for descending table scans of
`ORDER BY rowid DESC`-shaped queries, never a `Next`-style backward
*advance*), and the oracle's own `OP_Prev` is exactly this: `Last`'s
"position at the end" paired with a step-backward advance.

## Decision

Add four opcodes: `IdxRewind`/`IdxLast` (mirroring `Rewind`/`Last`'s
"jump to `P2` if empty" shape) and `IdxNext`/`IdxPrev` (mirroring
`Next`'s "jump to `P2` if another row was found" shape), all operating
on `CursorSlot::IndexRead` specifically rather than overloading
`Rewind`/`Last`/`Next` to also accept an index cursor — keeping the
existing table-cursor opcodes' match arms untouched and giving the
index-scan path its own, greppable opcode family. `IndexRead` itself
gains a persisted `IndexCursor` (previously each `SeekIndexEq` call
built a throwaway one) and a `current: Option<IndexRow>` traversal
position — the same shape `TableCursorState::current` already uses —
so `IdxNext`/`IdxPrev` have something to advance. `IdxRowid` (unchanged
mnemonic, extended semantics) now decodes whichever row `current` holds
rather than only a `SeekIndexEq` match, so it works for both the old
point-lookup path and the new scan path.

`src/btree/index.rs`'s `IndexCursor` gains `last()`/`prev()`, the exact
mirror of `first()`/`next()`'s depth-first stack walk: an interior
frame's forward action sequence (`step` 0..`2*num_cells+1`, even
`step` descends child `step/2`, odd yields entry `(step-1)/2`) turns
out to be symmetric under simple reversal — walking `step` from
`2*num_cells` down to `0` under the identical even/odd rule visits
every entry in the exact reverse order, so no separate encoding was
needed, just a countdown instead of a countup.

Like `SeekIndexEq`/`IdxRowid`/`NoConflict` before them (ADR history:
#243, #207), these four postdate the V2 oracle harvest — no query-time
index-ordered scan existed when `tools/opcodes-v2.json` was captured —
so they're excluded from `Opcode::ALL` but fully dispatched
(`src/vdbe/exec.rs`) and exhaustiveness-checked (`_exhaustive` in
`src/vdbe/program.rs`).

## Alternatives rejected

- **Overload `Rewind`/`Last`/`Next` to also accept `CursorSlot::IndexRead`,
  and add a bare `Prev`.** Rejected: blurs two genuinely different
  cursor kinds under one opcode, and `Prev` alone (without `IdxNext`)
  would leave the forward index-scan path silently reusing table-cursor
  opcodes against an index cursor — a type-confusable shape this
  codebase's `CursorTypeMismatch` errors are meant to catch, not paper
  over.
- **Give `SeekIndexEq` a "no probe, just position at the first/last
  entry" mode instead of new opcodes.** Rejected: conflates a point
  lookup (bounded probe, miss-or-hit) with a full sequential walk
  (unbounded, always succeeds until exhaustion) in one opcode's
  semantics, which would make both harder to reason about and to test
  in isolation.
- **Reuse the existing `IndexCursor::seek` linear-scan-from-start
  approach for a "walk from the beginning" instead of adding
  `last()`/`prev()`.** Rejected for the backward direction specifically:
  `ORDER BY <col> DESC` over an ascending index has no correct
  emulation via repeated forward seeks without either buffering the
  whole result (defeating the point of this ticket) or reversing the
  b-tree traversal itself.

## Consequences

- `Opcode::ALL` stays at 68; a future re-harvest that adds an
  `ORDER BY <indexed col> LIMIT n` query to `tools/harvest_opcodes.py`'s
  `QUERIES` list would be the natural point to fold these four in
  properly, the same way ADR-0018 folded in `Copy`/`AggStep`/
  `AggFinal`.
- `CursorSlot::IndexRead` is no longer a "no traversal position" slot
  (its doc comment already needs updating in the same PR that adds
  this ADR) — every future `IndexRead` consumer must be aware it now
  carries a live `IndexCursor` plus a `current` row, not just a root
  page number.
- The planner guardrail landing alongside these opcodes (#296's
  codegen half) is deliberately conservative: only a single-table,
  `WHERE`-free `SELECT ... ORDER BY <indexed col> [DESC] LIMIT n
  [OFFSET m]` takes this path; a `WHERE` clause, `WITHOUT ROWID` table,
  join, or multi-column `ORDER BY` beyond a matching index prefix all
  fall back to the existing sorter pipeline, unaffected by this ADR.
