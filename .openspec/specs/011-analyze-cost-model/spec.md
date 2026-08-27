---
domain: analyze-cost-model
version: 0.1.0
status: draft
date: 2026-08-24
---

# 011 — ANALYZE & Cost Model

The `ANALYZE` statement collects table/index statistics into `sqlite_stat1`,
and a cost model consumes those statistics to estimate the cost of a scan or
index probe. Together these let the query planner make cost-informed
decisions instead of the purely structural pattern-matching it uses today
(`src/codegen/select/join_access.rs::choose_join_access`).

Assigned to **V7** (`.openspec/plan.md:267`). Enables #470 (join ordering
heuristics), skip-scan optimization, and bloom-filter decisions — none of
which are in scope here.

Refs: #461

## Tier Position

`ANALYZE` and the cost model are optimizer-only features: their absence
never changes query *results*, only the plan chosen to produce them. They
sit outside the Tier 0 READ CORE and outside Tier 1 (parser)/Tier 2
(DML+DDL) — they land in **Tier 3**, alongside the other planner
refinements, and are additive to every fast path Requirement 16 of
`009-vdbe-codegen` already documents as "always wins, no ANALYZE/cost model
needed": those paths MUST keep working, unchanged, whether or not `ANALYZE`
has ever been run.

## Requirements

### Requirement 1: ANALYZE Statement [MUST]

`ANALYZE` and `ANALYZE table-name` MUST be accepted by the parser and
dispatched to a dedicated codegen path.

- `ANALYZE;` (no argument) analyzes every user table in the schema.
- `ANALYZE table;` analyzes only `table`.
- An unknown `table-name` MUST report a clean error (`no such table`),
  not panic and not silently no-op.
- `ANALYZE index-name` (real SQLite's other form, analyzing just the
  named index's owning table) is out of scope for this MVP and MUST
  report `Unsupported` rather than `Invalid` — it is syntactically valid
  SQL this parser doesn't yet implement, mirroring the `PRAGMA
  journal_mode` precedent (`src/parser/grammar.rs:952`).

**Implementation:** `src/parser/grammar.rs::parse_analyze_stmt`,
`src/parser/ast.rs::Analyze`, `src/codegen/analyze.rs::compile_analyze`

#### Scenario: Bare ANALYZE populates stats for every table

- GIVEN a schema with tables `t1` and `t2`, each containing rows
- WHEN `ANALYZE;` is executed
- THEN `sqlite_stat1` contains at least one row for `t1` and one row for
  `t2`

**Tests:** `tests/corpus/analyze_test.rs::bare_analyze_populates_all_tables`

#### Scenario: ANALYZE table-name scopes to one table

- GIVEN a schema with tables `t1` and `t2`
- WHEN `ANALYZE t1;` is executed
- THEN `sqlite_stat1` contains a row for `t1` and no row for `t2`

**Tests:** `tests/corpus/analyze_test.rs::analyze_single_table_scopes_stats`

#### Scenario: ANALYZE of an unknown table reports a clean error

- GIVEN a schema with no table named `ghost`
- WHEN `ANALYZE ghost;` is executed
- THEN the statement MUST fail with a `no such table` error, not panic

**Tests:** `tests/corpus/analyze_test.rs::analyze_unknown_table_reports_clean_error`

### Requirement 2: sqlite_stat1 Format [MUST]

`ANALYZE` MUST populate `sqlite_stat1(tbl TEXT, idx TEXT, stat TEXT)`,
created automatically (as an ordinary b-tree-backed table via the same
system-table path as `sqlite_master`, see `src/schema.rs`/
`src/btree/master.rs`) the first time `ANALYZE` runs, matching real
SQLite's `sqlite_stat1` shape (`sqlite3 src/analyze.c`).

- One row per table with `idx = NULL` and `stat = "<row-count>"`.
- One row per index on that table with `idx = <index-name>` and
  `stat = "<row-count> <index-rows-per-key...>"` (one integer per
  indexed column, matching real SQLite's `avg_eq` semantics: average
  number of rows sharing the same value for that column prefix).
- Re-running `ANALYZE` on a table MUST replace, not append to, its prior
  rows (`DELETE FROM sqlite_stat1 WHERE tbl = ?` semantics before
  re-inserting).

**Implementation:** `src/codegen/analyze.rs::compile_analyze`

#### Scenario: Re-running ANALYZE replaces stale stats

- GIVEN `ANALYZE t;` has already run once
- WHEN two more rows are inserted into `t` and `ANALYZE t;` runs again
- THEN `sqlite_stat1` has exactly one row per table/index for `t`
  reflecting the new row count, not two stale rows plus one fresh row

**Tests:** `tests/corpus/analyze_test.rs::re_analyze_replaces_stale_stats`

### Requirement 3: Stats and PlanCost Data Model [MUST]

A `Stats` type MUST decode a table's `sqlite_stat1` rows (spec 011/Req 2's
`(idx, stat)` shape) into an in-memory structure scoped to that one table,
and `PlanCost { estimated_rows: u64, estimated_io: u64 }` MUST be produced
by:

- `estimate_scan_cost(stats) -> PlanCost` — cost of a full table scan
  (rows = the table's row count from `Stats`; io = row count, since a
  scan touches one page-worth of I/O per row in this MVP's cost model).
- `estimate_index_cost(index_name, stats) -> PlanCost` — cost of an
  equality probe against `index_name`, using that index's stored
  `avg_eq` (`Stats` is already scoped to one table, so no separate
  `table`/`predicate` parameter is needed — the caller has already
  narrowed to a single-column equality candidate the same way
  `choose_join_access` does today, before ever consulting the cost
  model).

When `Stats` has no data for a table (no `ANALYZE` has run, or the table
was created after the last `ANALYZE`), both functions MUST return a
default estimate (`estimated_rows = u64::MAX` for a scan or an unknown
index, so an index probe with real stats is always preferred over one
without) rather than panicking or dividing by zero — this is what keeps
Requirement 16's stats-free fast paths behaviorally unaffected by this
spec.

**Implementation:** `src/planner.rs::Stats`, `src/planner.rs::PlanCost`,
`src/planner.rs::estimate_scan_cost`, `src/planner.rs::estimate_index_cost`

#### Scenario: Missing stats fall back to a conservative default

- GIVEN a table with no `sqlite_stat1` rows
- WHEN `estimate_scan_cost` is called for that table
- THEN it returns a `PlanCost` with `estimated_rows = u64::MAX`, not a
  panic or a division by zero

**Tests:** `src/planner.rs::missing_stats_fall_back_to_max_cost`

#### Scenario: An indexed equality is cheaper than a scan once stats exist

- GIVEN `ANALYZE` has recorded 10000 rows for table `t` and an index
  `idx_a` on `t(a)` with `avg_eq = 10`
- WHEN `estimate_scan_cost` and `estimate_index_cost` are compared for
  `WHERE a = ?`
- THEN `estimate_index_cost` reports fewer `estimated_rows` than
  `estimate_scan_cost`

**Tests:** `src/planner.rs::indexed_equality_cheaper_than_scan_with_stats`

### Requirement 4: Cost-Informed Join Access Selection [MUST]

`choose_join_access` (`src/codegen/select/join_access.rs:86`) MUST consult
`PlanCost` when `Stats` are available for a binding's table, and MUST fall
back to its current purely-structural selection (rowid → unique index →
full scan, unchanged) when they are not — so a database that has never run
`ANALYZE` behaves identically to today.

- The existing correctness/safety gating (`expr_is_safe_join_probe`,
  single top-level equality only, etc.) is unchanged; the cost model only
  changes *which* of the already-safe candidate accesses is preferred,
  never whether an access is safe to emit.
- This requirement does not add join *reordering* — join order stays
  FROM-clause order; see Requirement 5 (#470) for cost-informed reordering.

**Implementation:** `src/codegen/select/join_access.rs::choose_join_access`

#### Scenario: Cost model does not change behavior without stats

- GIVEN a fresh database with no `ANALYZE` ever run, and a join whose `ON`
  clause matches a `UNIQUE` index
- WHEN the join is compiled
- THEN the emitted access strategy is identical (opcode-for-opcode) to
  the pre-#461 behavior

**Tests:** `tests/corpus/analyze_test.rs::join_access_unchanged_without_analyze`

#### Scenario: Cost model can veto a unique-index seek stats show is not worth it

- GIVEN `ANALYZE` stats recording an unusually high `avg_eq` for a
  `UNIQUE` index (e.g. mostly-NULL/duplicate-tolerant data skewing the
  estimate), such that `estimate_index_cost` reports a higher
  `estimated_rows` than `estimate_scan_cost` for the same table
- WHEN a join's `ON` clause structurally matches that index
- THEN the full-scan fallback is chosen instead of the index seek

**Tests:** `tests/corpus/analyze_test.rs::cost_model_can_veto_expensive_index_seek`

### Requirement 5: Cost-Informed Inner/Cross Join Reordering [MUST]

`compile_select_joined_scan` (`src/codegen/select/joins.rs`) MUST reorder a
`FROM`-clause chain made entirely of `INNER`/`CROSS` joins (including
already-resolved `NATURAL`/`USING`) by ascending `estimate_scan_cost`
(`crate::planner`), scanning the smallest estimated table outermost — and
MUST leave the chain in original FROM-clause order whenever any join in it
is `LEFT`/`RIGHT`/`FULL` (reordering either side of an outer join changes
its result set) or whenever no table has an `ANALYZE`-derived cost (every
stats-free estimate is `u64::MAX`, so the cost-sort is a stable no-op and
execution order matches pre-#470 behavior byte-for-byte). A join's `ON`
constraint MUST be checked at the first execution level where every table
it references is bound, not assumed adjacent to its original FROM-clause
position. `EXPLAIN QUERY PLAN` (`src/codegen/select/eqp.rs`) MUST report
rows in the same reordered execution order this produces.

**Implementation:** `src/codegen/select/join_order.rs::plan_join_order`,
`src/codegen/select/joins.rs::compile_select_joined_scan`,
`src/codegen/select/eqp.rs::explain_query_plan`

#### Scenario: Join order is unchanged without ANALYZE

- GIVEN a fresh database with no `ANALYZE` ever run, and a plain `INNER
  JOIN` between two tables
- WHEN the join is compiled
- THEN the execution order matches original FROM-clause order, reported
  identically by `EXPLAIN QUERY PLAN`

**Tests:** `tests/corpus/analyze_test.rs::join_order_unchanged_without_analyze`

#### Scenario: Smaller table is scanned outermost once ANALYZE has run

- GIVEN `ANALYZE` stats recording table `t1` with far more rows than `t2`
- WHEN a plain `INNER JOIN` between `t1` and `t2` is compiled
- THEN `t2` (the smaller table) is scanned outermost, `EXPLAIN QUERY PLAN`
  reports that order, and the query still returns the same rows as the
  un-reordered plan

**Tests:** `tests/corpus/analyze_test.rs::join_order_reorders_by_analyze_row_counts`

### Requirement 6: Bloom-Filter Pre-Check for Unindexed Join Levels [MUST]

When a join level's single `ON` equality has no structural rowid/unique-
index seek available (`choose_join_access` returns `None`) and `ANALYZE`
records at least `join_access::MIN_ROWS_TO_BLOOM` rows for that level's
table, `compile_join_level_traverse` (`src/codegen/select/joins/level.rs`)
MUST preface that level's `Rewind`/`Next` scan with a one-time (`Once`-
guarded) `FilterAdd` pre-pass over the table's join-key column and a
per-outer-row `Filter` check that skips the scan entirely on a definite
miss. The underlying `FilterAdd`/`Filter` opcodes (`crate::vdbe::filter`)
MUST NOT produce a false negative for any `Value`: only an exact-match
`Value::Integer` key is ever hashed, and any other type (or a filter that
has ever seen one) always reports "maybe present" rather than risk
excluding a value real join semantics would still match. A database with
no `ANALYZE` history, or a table below the row threshold, MUST compile
with no `FilterAdd`/`Filter` opcode at all — byte-for-byte the pre-#464
program.

**Implementation:** `src/vdbe/filter.rs::BloomFilterState`,
`src/codegen/select/join_access.rs::choose_bloom_probe`,
`src/codegen/select/joins/level.rs::compile_join_level_traverse`

#### Scenario: Bloom pre-check is superseded by automatic indexing, not reachable today

- GIVEN `ANALYZE` stats recording a table with more rows than
  `MIN_ROWS_TO_BLOOM`, joined on a plain (non-indexed) equality (this
  scenario's original title, "Unindexed join level gets a Bloom pre-check
  once ANALYZE has run," assumed the Bloom path would actually compile in
  — since #545 that's no longer true)
- WHEN that join is compiled
- THEN `compile_join_level_traverse` tries `choose_auto_index_probe`
  first (`src/codegen/select/joins/level.rs`); because it gates on the
  same structural equality shape and the same row threshold as
  `choose_bloom_probe` (`MIN_ROWS_TO_AUTO_INDEX == MIN_ROWS_TO_BLOOM ==
  25`), any input that would satisfy the Bloom pre-check's conditions
  satisfies the automatic-index probe's conditions first — the program
  contains `OpenEphemeral`/`AutoIndexSeek`, never `FilterAdd`/`Filter`,
  for this exact case. The Bloom codegen path (`choose_bloom_probe`,
  `crate::vdbe::filter`) is retained as a defensive fallback — it would
  activate if the two thresholds or gating conditions ever diverged —
  but is not exercised by any reachable input today, so no scenario
  claims it "fires" in the current build.

**Tests:** `tests/corpus/analyze_test.rs::automatic_index_prefaces_unindexed_join_level_once_analyzed`

#### Scenario: Small or stats-free tables never get a Bloom pre-check

- GIVEN a table below `MIN_ROWS_TO_BLOOM` rows (with or without `ANALYZE`)
- WHEN a plain-equality join against it is compiled
- THEN the program contains no `FilterAdd`/`Filter` opcode

**Tests:** `tests/corpus/analyze_test.rs::bloom_filter_is_skipped_below_row_threshold`
