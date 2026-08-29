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
compound WHERE-clause expression trees, plus genuine range constraints
(`BETWEEN`, `LIKE`/`GLOB` prefix, `IN` lists) against an indexed column,
which needed two new VDBE opcodes (`SeekIndexGE`, `IdxCompareGT` —
ADR-0034) since those aren't reducible to a finite list of point probes
the way OR-to-IN is. OR-to-IN conversion itself did *not* need a new
opcode: an OR-chain of pure equalities converts to repeated probes of the
existing single-key seek opcodes (`SeekRowid`/`SeekIndexEq`), one per
value — see Requirement 2 and ADR-0033's revision note.

Refs: #605 (Requirements 1-2); #606/ADR-0034 (Requirement 3, range/prefix
seeks).

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

### Requirement 3: Range and Prefix Constraints [MUST]

`col BETWEEN lo AND hi`, `col LIKE 'prefix%'`/`col GLOB 'prefix*'`, and
`col IN (v1, ..., vN)` against an indexed column MUST compile to a genuine
index range/point-seek walk (`SeekIndexGE` + an `IdxCompareGT`-guarded
`IdxNext` loop for `BETWEEN`/`LIKE`/`GLOB`; a sequence of deduplicated
`SeekIndexEq` probes for `IN`) instead of a full scan + filter, whenever
every literal operand's storage class already matches the indexed
column's declared affinity (ADR-0034). A seek compares byte-for-byte
against what's actually stored, unlike the ordinary filter path's dynamic
comparison-affinity coercion — an affinity mismatch MUST fall back to the
unchanged filter lowering rather than risk a silently wrong seek.

- `BETWEEN`/`LIKE`/`GLOB` MUST include both boundaries correctly (the
  `LIKE`/`GLOB` prefix's upper bound is `prefix` with its last byte
  incremented, not a byte-identical duplicate — off by one here would
  either drop the last matching row or roll over into the next string in
  sort order).
- `IN` MUST deduplicate its value list before probing — a repeated value
  MUST NOT emit the same row twice.
- An affinity-mismatched literal operand (e.g. a string literal against
  an `INTEGER`-affinity column) MUST disqualify the fast path for that
  query, falling back to the ordinary scan, which still returns the
  correct rows via its own dynamic coercion.
- `EXPLAIN QUERY PLAN` MUST report `SEARCH ... USING INDEX` for all three
  shapes once the fast path is taken.

**Implementation:** `src/codegen/select/range_scan.rs::try_compile_between_seek`,
`src/codegen/select/range_scan.rs::try_compile_like_prefix_seek`,
`src/codegen/select/range_scan.rs::try_compile_in_list_seek`

#### Scenario: BETWEEN compiles to a bounded index range seek

- GIVEN a table `t(id, val)` with an index on `val`
- WHEN `SELECT id FROM t WHERE val BETWEEN 10 AND 20` is compiled
- THEN the program uses `SeekIndexGE`, and the returned rows are exactly
  those with `val` inside `[10, 20]` inclusive

**Tests:** `tests/unit/range_scan_test.rs::between_includes_both_boundaries`

#### Scenario: An affinity-mismatched BETWEEN operand falls back to the ordinary scan

- GIVEN the same table, `val` declared `INTEGER`
- WHEN the WHERE clause is `val BETWEEN '10' AND '20'` (string literals)
- THEN the program does not use `SeekIndexGE`, and the rows still match
  what the ordinary scan's dynamic affinity coercion would return

**Tests:** `tests/unit/range_scan_test.rs::between_falls_back_for_affinity_mismatched_string_operand`

#### Scenario: LIKE prefix compiles to a range seek with a correct upper bound

- GIVEN a table `t(id, name)` with an index on `name`, including a row
  whose value (`'fop'`) is the lexicographic successor of every `'foo...'`
  string
- WHEN `SELECT id FROM t WHERE name LIKE 'foo%'` is compiled
- THEN the program uses `SeekIndexGE`, matches `'foo'`/`'foobar'`, and
  never includes the `'fop'` row

**Tests:** `tests/unit/range_scan_test.rs::like_prefix_matches_bare_prefix_and_extended_strings`,
`tests/unit/range_scan_test.rs::like_prefix_excludes_the_lexicographic_rollover_row`

#### Scenario: GLOB prefix takes the same fast path as LIKE

- GIVEN the same table
- WHEN `SELECT id FROM t WHERE name GLOB 'foo*'` is compiled
- THEN the program uses `SeekIndexGE` and returns the same rows the
  equivalent `LIKE 'foo%'` query would

**Tests:** `tests/unit/range_scan_test.rs::glob_prefix_matches_like_the_asterisk_form`

#### Scenario: IN list dedupes values and probes once per distinct value

- GIVEN a table `t(id, val)` with an index on `val`
- WHEN `SELECT id FROM t WHERE val IN (5, 5, 5)` is compiled
- THEN exactly one row is returned, not three copies of it

**Tests:** `tests/unit/range_scan_test.rs::in_list_matches_exactly_the_listed_values`,
`tests/unit/range_scan_test.rs::in_list_with_duplicate_values_does_not_duplicate_the_row`

#### Scenario: EXPLAIN QUERY PLAN reports index usage for all three shapes

- GIVEN indexed tables for `BETWEEN`, `LIKE` prefix, and `IN`
- WHEN each query is passed to `explain_query_plan`
- THEN every plan row's detail contains `SEARCH` and `USING INDEX`

**Tests:** `tests/unit/range_scan_test.rs::explain_query_plan_reports_between_as_a_search_using_index`,
`tests/unit/range_scan_test.rs::explain_query_plan_reports_like_prefix_as_a_search_using_index`,
`tests/unit/range_scan_test.rs::explain_query_plan_reports_in_list_as_a_search_using_index`
