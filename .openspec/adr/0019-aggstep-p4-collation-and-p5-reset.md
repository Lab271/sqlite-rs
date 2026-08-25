# 0019 — `AggStep` gains a collation-carrying `P4` and a `P5` reset flag

**Status:** Accepted · **Date:** 2026-08-21

## Context

#263 asked `src/codegen/select/aggregate.rs`'s aggregate compilation (plain
aggregates, `GROUP BY`, `HAVING`) to stop hand-rolling per-kind register
arithmetic (`AggKind`/`AggSlot`'s `primary`/`aux` registers) and instead
emit `Opcode::AggStep`/`Opcode::AggFinal` — already in the harvested/
frozen `Opcode::ALL` set since ADR-0018, but never emitted by codegen
until now. ADR-0018 explicitly flagged this as the follow-up a future
ticket would need.

Rerouting surfaced two gaps the ticket's "pure internal consolidation,
no new correctness surface" framing did not anticipate:

1. `crate::vdbe::aggregate::step`'s `Min`/`Max` arms hardcoded
   `Collation::Binary` — the same class of bug #265 had just fixed in
   the old register-arithmetic scheme's `Lt`/`Gt` compares. Rerouting
   onto `AggStep` unfixed would have **regressed** #265 for every query
   whose aggregate compilation now goes through it.
2. `Vm::agg_contexts` (`Vec<Option<AggState>>`, keyed by `AggStep`/
   `AggFinal`'s `P1`) had no reset primitive. A `GROUP BY`'s per-row loop
   reuses the same compile-time slot number across every runtime group;
   without a way to say "start over," the second group's accumulator
   would silently continue from the first group's final state.

## Decision

**Collation:** add `P4::AggFunc { name: String, arity: usize, collation:
Collation }`, used by `AggStep` in place of the plain `P4::Str("name(arity)")`
descriptor it shared with `Function`. `AggFinal` keeps `P4::Str` — it only
reads an already-finalized value, no comparison to collate.
`crate::vdbe::aggregate::step` gained a `collation: Collation` parameter,
threaded into its `Min`/`Max` arms' `compare(...)` calls in place of the
hardcoded `Collation::Binary`. Codegen resolves it via `collation_of`
(#265's resolution: an explicit `x COLLATE name` wrapper only) exactly
like the scalar comparison path.

Deliberately **not** carried: a comparison *affinity*.
`crate::vdbe::aggregate::step`'s `compare` call has no affinity parameter
to feed one to — affinity coercion (`apply_affinity` on operand copies
before comparing, per spec 008 Requirement 1) has never been part of the
`AggStep`/`AggFinal` contract. This is a pre-existing gap in that opcode
pair, not something this ticket introduces or regresses (the old
register-arithmetic scheme's `Lt`/`Gt` had no affinity handling before
#265 either, and #265 only added collation). Left as a known, tracked
gap for a future ticket, same disposition ADR-0018 gave the original
"unemitted" gap.

**Reset:** `AggStep`'s `P5` (previously always `0`, unused by any
existing test or codegen) is now a reset flag — nonzero discards the
slot's prior state before folding this call's args, identical to the
slot never having been stepped. Codegen sets it on exactly one `AggStep`
per aggregate slot per group: the boundary row that starts a new group.
Every other row in the group folds with `P5 = 0`. No new opcode was
needed — `P5` is already documented as a general per-opcode flags
operand (`Instruction`'s doc comment), and every pre-existing `AggStep`
call site (hand-assembled tests, now codegen) passed `P5: 0` already, so
this is an additive semantic, not a breaking one.

## Alternatives rejected

- **A dedicated `AggReset` opcode.** Rejected: reopens the harvest
  question ADR-0015/ADR-0018 already went through, for a single-bit
  concern `P5` already covers for free. `P5` existing specifically as an
  unused flags operand on this instruction made it the smaller change.
- **A fresh slot number per group at compile time.** Not viable: the
  number of runtime groups is unknown at compile time (`GROUP BY`'s
  per-row loop is a single emitted instruction sequence executed once
  per sorted row) — slot numbers are baked into the bytecode, so they
  cannot vary per runtime iteration without a reset mechanism regardless.
- **Add affinity to `AggFunc`'s `P4` and thread it through
  `aggregate::step` in this same ticket.** Deferred: not required by
  #263 or #265, and doing it properly means giving `aggregate::step` the
  same operand-copy-then-coerce shape `emit_compare_false_jump` uses,
  which is its own scoped change. Tracked as a follow-up instead of
  scope-creeping this ticket.
- **Fold plain (non-`GROUP BY`) aggregates like `SELECT count(*) FROM t`
  into this same ticket**, now that `AggStep`/`AggFinal` make it nearly
  free (a single always-open "group"). Deferred to a follow-up ticket to
  keep this refactor's diff reviewable against #239/#242's existing
  `GROUP BY` test coverage.

## Consequences

- `src/codegen/select/aggregate.rs`'s `AggKind`/`AggSlot`'s `primary`/`aux`
  register-arithmetic (`reset_agg`, `accumulate_agg`) is retired.
  `AggSlot` now carries a `slot: i32` (an `Vm::agg_contexts` index, a
  disjoint address space from the register file) instead of registers.
  `flush_group`'s `avg` special-casing (`RealAffinity` + `Divide`) is
  gone — `crate::vdbe::aggregate::finalize` already divides internally.
- `crate::vdbe::exec::agg_step` requires `P4::AggFunc`; a plain
  `P4::Str` (still valid for `AggFinal`) is now a malformed-instruction
  error for `AggStep`. No production code emitted `P4::Str` for
  `AggStep` before this ticket (it was never emitted at all outside
  hand-assembled tests), so nothing outside this repo's own test suite
  is affected — those hand-assembled tests were updated in the same
  commit.
- `tools/opcodes-v2.json`'s recorded `p4_variants` for `AggStep`
  (`"count(0)"`, `"sum(1)"`, oracle-observed EXPLAIN strings) describe
  the *oracle's* P4 rendering, not this implementation's internal `P4`
  representation — they are unaffected; no re-harvest needed. This
  implementation's own `EXPLAIN` rendering of `P4::AggFunc` (`"name(arity)-COLLATION"`)
  is not yet exercised by any parity/EXPLAIN-diffing test (spec 009's
  `tests/unit/vdbe_explain_test.rs` has no `AggStep`/`AggFinal` case), so no
  existing byte-for-byte contract is broken; a future EXPLAIN-parity
  ticket for aggregate opcodes should decide then whether to match the
  oracle's plain descriptor instead.
