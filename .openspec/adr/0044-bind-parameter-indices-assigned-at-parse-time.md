# ADR-0044: Bind-parameter indices are assigned at parse time, in text order

**Date:** 2026-09-10
**Status:** Accepted

## Context

A bare `?` has no index written in the SQL; something has to assign one.
SQLite does this while parsing, in `sqlite3ExprAssignVarNumber`: a bare `?`
takes one more than the highest index used so far, and an explicit `?NNN`
raises that high-water mark. The index is therefore a property of the SQL
text.

This crate assigned it during code generation instead, from a `next_param`
counter on `RegAlloc` (`src/codegen.rs`), read by `compile_value`. That made
the index a property of *compilation*, and two things about compilation broke
it:

- **Codegen does not visit expressions in text order.** `compile_update`
  compiles the `WHERE` operand before the `SET` assignments, because the scan
  has to be positioned before the row body is emitted. So
  `UPDATE t SET v = ? WHERE k = ?` numbered the `WHERE` placeholder 1 and the
  `SET` placeholder 2, and a caller binding in text order had its values
  swapped.
- **There is more than one `RegAlloc` per statement.** Eight sites call
  `RegAlloc::new()`, each starting the counter at zero. A plan that compiles
  part of a statement through a second allocator — the covering-index seek
  path, for instance — restarted numbering mid-statement, collapsing two
  distinct placeholders onto index 1.

Both were silent. The swapped `UPDATE` matched no row and returned `Ok(0)`,
which is also the rows-affected value an optimistic-concurrency check reads
as "someone else won the race" — so a compare-and-swap built on it failed
100% of the time while looking like ordinary contention. Both were reported
by the first consumer to drive the embedding API with `?` rather than `?NNN`.

Explicit `?NNN` was unaffected, which is why the existing parameter tests
missed it: they were written with `?1`/`?2`, the form this repository's own
code writes. `sqlx` — and most drivers — emit bare `?`.

## Decision

Assign the index in the parser, in text order, and carry it on the AST:
`ParamKind::Anonymous(u32)`. `Parser` holds one `next_param` high-water mark,
which is per-statement by construction because every parse entry point builds
its own `Parser` for one statement's tokens. `?NNN` raises the mark; a
following bare `?` continues past it. Codegen reads the index and no longer
owns a counter, so `RegAlloc::anonymous_param` and
`RegAlloc::numbered_param` are deleted along with the field they mutated.

## Alternatives rejected

**A numbering pass over the AST before codegen.** Leaves the AST shape and
the parser untouched, and would fix both reported cases. Rejected because it
needs a visitor that reaches every expression position in every statement
type — `SET`, `WHERE`, `VALUES`, projections, `JOIN ON`, `HAVING`, `LIMIT`,
subqueries, CTEs — and a position the visitor misses is not a compile error.
It is this same bug, silently, in a shape nobody has tested yet. Assigning at
the point of parse makes "was this numbered?" unrepresentable rather than
merely tested.

**Refusing bare `?` at prepare, the way named parameters are refused.**
Provably safe and two lines. Rejected because bare `?` is what drivers
generate: refusing it does not protect a consumer, it excludes them. Refusing
named parameters is defensible because there is no index to bind them to;
here the index exists and was simply computed in the wrong place.

**Keeping the counter in codegen and making `compile_update` visit the `SET`
list first.** Fixes the one reported statement and leaves the mechanism —
plan-order-dependent numbering across eight allocators — in place for the
next plan to trip over.

## Consequences

- `ParamKind::Anonymous` carries a `u32`. Five codegen match sites that
  already accepted `Numbered(_)` alongside it needed `Anonymous(_)`; the
  printer still renders `?`, because the source form is what it round-trips.
- Numbering no longer depends on which plan the optimizer chose. This is the
  substantive gain: it was previously possible for the same SQL to number its
  parameters differently after an unrelated planner change, with no test
  failing.
- `Program::param_count()` (max `P1` over `Opcode::Variable`) becomes
  trustworthy for bare `?`. It was reporting 1 for a two-placeholder
  statement whenever the indices collapsed.
- Parse-time assignment means a statement that never reaches codegen still
  has its parameters numbered. That is what SQLite does and it is what
  `sqlite3_bind_parameter_count` reports after `prepare`.
