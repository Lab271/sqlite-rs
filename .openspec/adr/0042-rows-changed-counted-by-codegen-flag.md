# 0042 — Codegen decides which mutation is a row change, and `None` is not zero

**Status:** Accepted · **Date:** 2026-09-04

## Context

Spec 013 Requirement 1 asks for `sqlite3_changes()`: how many rows the last
`INSERT`/`UPDATE`/`DELETE` changed. Spec 013 calls it the one item on its list
a consumer cannot work around, because without it a caller cannot distinguish
an `UPDATE` that matched from one that did not, and that distinction is what
every optimistic-concurrency scheme is built on.

The obvious implementation — increment a counter in the `Insert` and `Delete`
opcode handlers — is wrong, and measurably so on the tree at 0.18.10:

| statement | opcodes emitted per row | counted naively |
|---|---|---|
| `INSERT` | `Insert` | 1 |
| `DELETE` | `Delete` | 1 |
| `UPDATE`, single-pass | `Delete` + `Insert` (`update.rs:603,605`) | **2** |
| `UPDATE`, two-pass range-seek | ephemeral `Insert` (`update.rs:276`) + `Delete` + `Insert` | **3** |

The two-pass plan is #666/#675's range-seek path, which stashes matched rowids
in an ephemeral b-tree using the same `Opcode::Insert`. So the same `UPDATE`
would report 2 or 3 depending on which plan the optimizer picked, and neither
is 1. Index maintenance (`IdxInsert`, `IdxDelete`, `AutoIndexInsert`) has the
same character: a write, adjacent to a row, that is not a row change.

The opcode does not carry enough information to answer. Codegen does.

## Decision

**Codegen marks the one mutation that is the row change, with
`OPFLAG_NCHANGE` (`0x01`) on `P5`.** Same bit and same job as stock SQLite's
flag of that name. `cursor::insert`/`cursor::delete` increment `Vm`'s counter
only when it is set; `Instruction::with_p5` is the constructor that sets it,
alongside the existing `with_p4`. `P5` was unread by both opcodes, so nothing
had to move.

An `UPDATE` flags its `Insert` and not the paired `Delete` — one changed row,
counted once. `INSERT` flags its table `Insert`; `DELETE` flags both of its
`Delete` sites. Nothing else is ever flagged.

**The count is exposed as `Option<u64>`, and `None` is not `Some(0)`.**
`StepOutcome::changes` is `Some(n)` when the program is a counting statement
and `None` when it is not:

- `Some(0)` means "this was an `INSERT`/`UPDATE`/`DELETE` and it changed
  nothing" — a lost optimistic-concurrency race, which is the case the
  requirement exists to make visible.
- `None` means "not that kind of statement", so a connection tracking
  `sqlite3_changes()` leaves its stored count alone.

The discriminator is **static** — `Program::counts_changes()` asks whether the
program *contains* a flagged instruction, not whether one executed. An
`UPDATE` whose `WHERE` matches nothing never runs its flagged `Insert` but
must still report `Some(0)`.

**`execute_transaction_step` becomes a wrapper** over
`execute_transaction_step_counted`, which returns the count. Same pattern
ADR-0040 settled on for streaming: one loop, the older signature expressed in
terms of the newer one, so the two cannot drift and the existing suite is the
equivalence proof.

## Alternatives rejected

- **Count in the handlers, unconditionally.** Reports 2 or 3 for a one-row
  `UPDATE`, plan-dependently, and counts index maintenance. This is the
  alternative the table above exists to close, and
  `update_of_one_row_reports_one_under_both_plans` is its regression guard:
  removing the flag check fails that test and two others.
- **Return `u64` and let `0` mean both.** Collapses "changed nothing" into
  "not a counting statement", which is exactly the distinction SQLite's
  retention rule is built on — a `SELECT` would zero a count that should have
  survived it. The two-case type is the whole point and should not be
  simplified away.
- **A `Program { counts_changes: bool }` field set by codegen.** Equivalent
  in behaviour, but it can disagree with the instructions it describes, and
  `Program::new` has many call sites. Deriving it costs one pass over a
  handful of instructions.
- **Change `execute_transaction_step`'s return type in place.** Ten call
  sites across `src/bin/`, tests, benches and examples, for a value almost
  none of them want. The wrapper is free.
- **Track the count across statements in the `Vm`.** A `Vm` lives for one
  statement, so it cannot. Cross-statement retention is the connection's
  rule and belongs to spec 013/Req 1's `Connection::changes`.

## Consequences

The number is correct for the statement just run, verified against the pinned
3.53.4 oracle's own `changes()` for a thirteen-statement sequence covering
both `UPDATE` plans, a miss, a partial `DELETE` and a full one
(`tests/corpus/changes_oracle_test.rs`). Both wrong designs above were
mutation-checked against that test as well as the unit suite.

`Connection::changes` is still absent — this is the engine half. What the
facade has left to do is one line: store the value on `Some`, ignore `None`.

Adding a `P5` flag reopens no frozen set: no new opcode, so ADR-0015, ADR-0018
and ADR-0020 are untouched. But `P5` on `Insert` is now meaningful where its
doc comment previously said conflict-resolution flags were "not modeled", so a
future `OR REPLACE`/`OR IGNORE` implementation must pick bits other than
`0x01`.
