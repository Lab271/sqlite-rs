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
compound WHERE-clause expression trees. It does not cover genuine range
constraints (`<`, `>`, `BETWEEN`, `LIKE` prefix) — those require new VDBE
range-seek opcodes and are tracked separately in #606. OR-to-IN conversion
turned out *not* to need a new opcode: an OR-chain of pure equalities
converts to repeated probes of the existing single-key seek opcodes
(`SeekRowid`/`SeekIndexEq`), one per value — see Requirement 2 and
ADR-0033's revision note.

Refs: #605 (Requirements 1-2, this spec's implemented scope); #606 tracks
Requirement 3 separately and is being picked up on a different branch.

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

### Requirement 2: OR-to-IN Conversion [MUST]

A top-level `OR`-chain of at least two equalities, all against the same
column and each against a supported literal/bind-parameter operand, MUST
convert to one probe per value using the existing single-key seek
opcodes — no new opcode needed, since each value is still a point lookup.

- `x = 1 OR x = 2 OR x = 3` MUST let the rowid-seek and covering-index-scan
  fast paths probe once per value instead of falling back to a full scan.
- Any disjunct that isn't a pure equality against the exact same column
  (a different column, a compound sub-condition, an unsupported operand)
  MUST disqualify the whole chain — a fast path built only for point
  probes must never silently drop a disjunct it can't also enforce.
- Skip-scan (non-leading indexed column) and join-ON equality are out of
  scope for this requirement; only the rowid-seek and covering-index-scan
  paths convert.

**Implementation:** `src/codegen/select/limit_scan.rs::or_chain_equality_operands`

#### Scenario: OR-chain of rowid equalities converts to repeated seeks

- GIVEN a table `t(a, b)` with rows `1..5`
- WHEN `SELECT a, b FROM t WHERE rowid = 1 OR rowid = 3 OR rowid = 5;` is
  compiled
- THEN the program contains three `SeekRowid` instructions and no
  `Rewind`, and the rows match the pinned oracle exactly

**Tests:** `tests/corpus/or_to_in_test.rs::rowid_or_chain_seeks_and_matches_oracle`

#### Scenario: OR-chain against an indexed column converts via the covering-index path

- GIVEN a table `t(a, b)` with an index on `a`
- WHEN `SELECT a FROM t WHERE a = 1 OR a = 3 OR a = 5;` is compiled
- THEN the program contains three `SeekIndexEq` instructions and no
  `Rewind`, and the rows match the pinned oracle exactly

**Tests:** `tests/corpus/or_to_in_test.rs::covering_index_or_chain_seeks_and_matches_oracle`

#### Scenario: A mixed-column OR chain cannot convert

- GIVEN the same table
- WHEN the WHERE clause is `a = 1 OR b = 20` (two different columns)
- THEN no conversion happens — the ordinary scan is used, and results
  still match the pinned oracle exactly

**Tests:** `tests/corpus/or_to_in_test.rs::mixed_column_or_falls_back_to_ordinary_scan_and_matches_oracle`

### Requirement 3: Range and Prefix Constraints [Future]

`LIKE`/`GLOB` prefix ranges, a single-seek `BETWEEN`, and multi-seek `IN`
lists (#606) need a new range-seek opcode, since those aren't reducible to
a finite list of point probes the way OR-to-IN is. Tracked in #606.
