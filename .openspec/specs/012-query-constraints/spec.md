---
domain: query-constraints
version: 0.1.0
status: draft
date: 2026-08-28
---

# 012 — WHERE-Clause Constraint Extraction

The seek/probe fast paths in `src/codegen/select/limit_scan.rs` and
`src/codegen/select/join_access.rs` (rowid seek, covering-index scan,
skip-scan, join-access equality) each recognize only a single top-level
`column = <literal|param>` equality in the `WHERE`/`ON` clause
(`top_level_equality_operands`) — any `AND`/`OR` compound condition falls
straight through to the ordinary full scan, even when the compound
condition provably fixes a value for the seekable column.

This spec covers extracting additional index-eligibility information from
compound WHERE-clause expression trees. It does not cover range constraints
(`<`, `>`, `BETWEEN`, `LIKE` prefix) or multi-value seeks (`IN`, `OR`→`IN`)
— those require new VDBE range-seek opcodes and are tracked separately
(#606, and the OR→IN half of #605).

Refs: #605, #606

## Tier Position

Constraint extraction is optimizer-only: its absence never changes query
*results*, only whether a fast seek/probe path or the ordinary scan answers
a query. It sits in **Tier 3** alongside the other planner refinements
(see `011-analyze-cost-model`'s Tier Position for the same argument).

## Requirements

### Requirement 1: Constant Propagation Through AND-Conjunctions [MUST]

When a `WHERE`/`ON` clause is a top-level `AND`-conjunction of equalities,
a column that is transitively equal (through a chain of `column = column`
and `column = <literal|param>` conjuncts) to a literal or bind parameter
MUST be treated as equal to that value by every existing single-equality
fast path (rowid seek, covering-index scan, skip-scan, join-access
equality) — without changing the WHERE clause's own semantics.

- `a = b AND b = 5` MUST let a fast path that probes on `a` use the
  literal `5`.
- A multi-hop chain (`a = b AND b = c AND c = 5`) MUST resolve the same
  way.
- Any `OR` in the top-level clause, or a non-equality conjunct touching
  the same column, MUST prevent propagation for that column (propagation
  only ever narrows what a fast path may additionally recognize; it never
  changes which rows the ordinary scan's `WHERE` evaluation returns).

**Implementation:** `src/codegen/select/limit_scan.rs::propagate_constants`

#### Scenario: Direct equality chain enables a rowid seek

- GIVEN a table `t(a, b, name)` with no explicit `WHERE rowid = <int>`
  form
- WHEN `SELECT a, b, name FROM t WHERE rowid = a AND a = 2;` is compiled
- THEN the program uses `SeekRowid`, not `Rewind`/`Next`, and returns the
  same row a full scan would

**Tests:** `tests/unit/codegen_select_test.rs::rowid_equality_propagates_through_and_conjunction`

#### Scenario: OR prevents propagation

- GIVEN the same table
- WHEN the WHERE clause is `a = b OR b = 5` (an `OR`, not an `AND`)
- THEN no column resolves to a constant — the ordinary scan is used

**Tests:** `src/codegen/select/limit_scan.rs::tests::propagate_constants_ignores_or`

### Requirement 2: OR-to-IN Conversion [Future]

`x = 1 OR x = 2 OR x = 3` (equality chains on the same column, pure
equality terms only) SHOULD convert to a multi-value seek. Deferred: needs
a new range/multi-seek VDBE opcode (frozen inventory, `009-vdbe-codegen`).
Tracked as the remaining half of #605.

### Requirement 3: Range and Prefix Constraints [Future]

`LIKE`/`GLOB` prefix ranges, a single-seek `BETWEEN`, and multi-seek `IN`
lists (#606) all need the same new range-seek opcode as Requirement 2.
Tracked in #606.
