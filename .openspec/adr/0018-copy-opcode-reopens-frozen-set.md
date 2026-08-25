# 0018 — `Copy` (and `AggStep`/`AggFinal`) reopen the frozen V2 opcode set

**Status:** Accepted · **Date:** 2026-08-21

## Context

#141 needed a `Copy`/`SCopy`/`Move`/`IntCopy`-family opcode so
`compile_row_values` and `emit_branch_into` (`src/codegen/select.rs`,
`src/codegen/expr.rs`) could relocate an already-computed value into a
reserved or shared register, instead of refusing any query with two
computed result columns or a compound CASE branch. A prior investigation
(#141's own comment thread) found no V2-scope single-table query shape
ever makes the pinned oracle's real register planner emit `Copy` — it
always pre-plans a computed expression's destination register. `Copy`
only appears in the oracle's own plans for aggregate/GROUP BY/
compound-SELECT shapes, which #234 (V4) has now landed.

Separately, `Opcode::Copy` was already hand-added to the `Opcode` enum
(not `Opcode::ALL`, not `tools/opcodes-v2.json`) during #208
(`INSERT ... SELECT`) and is already in production use across
`src/codegen/{insert,subquery,select}.rs` — a quiet violation of the
harvest-not-hand-add discipline `tools/opcodes-v2.json`/
`tests/unit/vdbe_opcode_completeness_test.rs` exist to enforce. `AggStep`/
`AggFinal` are in the same state from #241/#242: enum variants with real
`src/vdbe/exec.rs` handlers and hand-assembled unit-test coverage, but
never harvested or added to `Opcode::ALL` — `src/codegen/select.rs`'s
actual aggregate compilation (the `agg.primary`/`agg.aux` dest-block
pattern) computes sums/counts a different way and never emits them.

## Decision

Re-run the harvest (`tools/harvest_opcodes.py`, pinned oracle 3.53.4)
with `SELECT count(*), sum(price) FROM products` added to the V2 query
set — the minimal shape that reaches `Copy` in the oracle's plan without
also dragging in GROUP BY's five unrelated control-flow opcodes
(`Compare`/`Gosub`/`If`/`Jump`/`Move`, none needed by this ticket). That
query's plan also includes `AggStep`/`AggFinal`, so all three join the
frozen set at once (65 → 68), formally closing the harvest gap for all
three rather than only the one this ticket strictly needed. `Opcode::ALL`
gains `Opcode::AggStep`, `Opcode::AggFinal`, `Opcode::Copy` to match.

`AggStep`/`AggFinal` remain unemitted by codegen after this change —
that gap (aggregate compilation not actually routing through them) is
unaffected and not addressed here; this ADR only legitimizes their
presence in the harvested/frozen set, matching what was already true of
the `Opcode` enum and its exhaustiveness check.

`Copy`'s semantics: `r[P2] = r[P1]` (a single-register copy — `SCopy`/
`Move`/`IntCopy`'s more specific oracle semantics are not implemented;
nothing in this ticket's scope needs them). `compile_row_values` now
computes every result column first (wherever the bump allocator lands
it), checks whether the run is already contiguous (the common case,
unchanged bytecode), and only when it is not, reserves a fresh
contiguous block and `Copy`s each value into place. `emit_branch_into`'s
fallback arm does the analogous thing for a CASE branch that is not a
bare literal/column reference: compile via `compile_value`, then `Copy`
into the branch's shared destination register. The same reserve-after-
compute-then-copy shape already existed for `Function`'s multi-arg
contiguity check and for the aggregate/snapshot record dest-block
(`src/codegen/select.rs`) — this ticket generalizes it to the two
remaining call sites that used to reject instead.

## Alternatives rejected

- **Leave `Copy` hand-added, close #141 as "already implemented by
  #208."** Rejected: #208's `Copy` usage never touched
  `compile_row_values`'s contiguity check or `emit_branch_into` — the
  six queries in #141's own repro still failed against a pre-fix build.
  The opcode existing in the enum did not mean the bug was fixed.
- **Skip harvesting, just add `Copy` to `Opcode::ALL` from its existing
  hand-added state.** Rejected for the same reason #141's acceptance
  criteria insists on a harvest: `Opcode::ALL`/`opcodes-v2.json` is
  supposed to be an oracle-traceable inventory, not a hand-authored
  guess. Harvesting now also surfaces (and formally closes) the
  pre-existing `AggStep`/`AggFinal` gap for free.
- **Harvest via the GROUP BY shape the original investigation tried.**
  Rejected per that investigation's own finding: it pulls in five
  unrelated opcodes with no ticket driving their implementation.

## Consequences

- `Opcode::ALL` is now 68 entries; `tools/opcodes-v2.json`'s
  `opcode_count`/`query_count` moved 65→68, 38→39.
  `tools/harvest_opcodes.py`'s `QUERIES` list permanently carries the
  new aggregate query, so a future re-harvest keeps `Copy`/`AggStep`/
  `AggFinal` in scope.
- `compile_row_values` no longer has a hard contiguity-rejection path;
  `emit_branch_into` accepts arbitrary CASE branch expressions.
  `src/codegen/expr.rs`'s `FunctionCall` argument compilation gained the
  same reserve-and-copy fallback for the same underlying reason
  (`coalesce`/`ifnull` with another multi-register-producing argument
  next to them hit the identical contiguity check under a different
  name).
- `AggStep`/`AggFinal` staying unemitted by codegen is a known, tracked
  gap (not introduced here) — a future ticket that wants codegen to
  actually route aggregate compilation through them, rather than the
  existing `agg.primary`/`agg.aux` dest-block scheme, would need to
  either wire them in or retire them from the enum.
