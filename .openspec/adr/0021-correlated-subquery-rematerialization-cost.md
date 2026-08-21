# 0021: Defer a coroutine rewrite for correlated subqueries; scope an incremental hoist instead

Date: 2026-08-21
Status: Accepted

## Context

`src/codegen/subquery.rs`'s module doc states the design plainly:
"materialization only (no coroutines)" — every subquery occurrence
(scalar, `IN`, `EXISTS`) opens its own cursor (and, for `IN`, an
ephemeral index) and re-runs its inner scan inline at the exact point
the enclosing expression is evaluated. For an **uncorrelated**
subquery this was pure waste — the result never changes across outer
rows — and #306 fixed that case: a static correlation check
(`subquery_is_correlated` in `subquery.rs`) now lets the codegen hoist
an uncorrelated `WHERE`-clause scalar/`IN` subquery's materialization
to once, before the outer scan's `Rewind`, instead of once per row.

This ticket (#303) is about what's left: a **correlated** subquery
(one that references an outer-query column) is not eligible for #306's
hoist — SQLite's semantics require it to re-evaluate for every distinct
combination of outer-referenced values — but the current implementation
re-evaluates it for *every outer row*, even when many rows share the
same correlated value(s). For a correlated subquery/join nested inside
a large outer scan, this is `O(outer_rows × inner_scan_cost)` work where
a keyed cache could make repeat correlated values `O(1)` after the first
occurrence.

## Decision

**Do not attempt a full coroutine-based re-execution model in this
pass.** SQLite's own approach (`OP_InitCoroutine`/`OP_Yield` driving a
subquery as a separate co-routine that the outer loop pulls one row from
at a time) is a materially different VDBE execution model than this
codebase's current "inline materialize" approach — introducing it would
touch cursor lifecycle, register-frame management, and every
`compile_*_scan` call site, not just `subquery.rs`. That is out of
proportion to the evidence in hand (see Benchmark below) and belongs to
a dedicated, separately-scoped ticket if the benchmark data from a real
workload ever demonstrates it's needed.

**Scope the actual follow-up narrowly: memoize a correlated subquery's
result keyed on its outer-referenced value(s), reusing the exact same
building blocks #306 already built** — `subquery_is_correlated`'s AST
walk (extended to also *collect* which outer columns a correlated
subquery references, not just whether any exist), and the
materialize/test split #306 introduced (`materialize_in_subquery_index`
and the scalar-subquery hoist path). Sketch:

1. When a subquery is correlated but the outer-referenced column set is
   small and known ahead of the scan (i.e. resolvable via
   `subquery_is_correlated`'s walk), allocate a small in-VDBE cache
   (an ephemeral index keyed on the outer value(s), holding the
   subquery's result) instead of a single hoisted register/cursor.
2. Per outer row: probe the cache first (`Found`-style test on the
   outer value(s)); on a cache miss, run the existing per-row
   materialization logic once and insert the result into the cache
   before using it; on a hit, skip straight to using the cached result.
3. This degrades gracefully to "no better than today" when the
   correlated value has high cardinality (every row misses), and wins
   proportionally to how much repetition exists in the correlated
   value(s) — which is the common case for a correlated subquery
   filtering on a low-cardinality outer column (a bucket, a category, a
   foreign key into a small dimension table).
4. Explicitly NOT attempted here: correlated subqueries inside a
   multi-level `JOIN` (`compile_join_level`/`emit_join_final_row`) — the
   cache-keyed approach above targets single-table `WHERE`-clause
   correlated subqueries first, same scope boundary #306 drew. A join
   extension is a further follow-up once the single-table case is
   proven out.

## Alternatives rejected

- **Full `OP_InitCoroutine`/`OP_Yield` rewrite** (SQLite's real
  mechanism): rejected for this pass — see Decision. Revisit only if a
  real workload's benchmark shows the memoization approach above is
  insufficient (e.g. correlated value cardinality is uniformly high in
  practice, defeating the cache).
- **Do nothing, leave as documented "materialization only" tradeoff**:
  rejected — #243's planner/`EXPLAIN QUERY PLAN` work in the same
  0.13.0 release is explicitly about avoiding this class of cost
  elsewhere in the planner, so leaving this specific case unaddressed
  reads as an inconsistency, and the benchmark below shows a real,
  currently-unbounded cost (see Consequences).

## Benchmark: current cost

`tests/performance/engine.rs` gained a `correlated_subquery` scenario
(guarded to run only against the `bench_1mb.db` fixture, not
`bench_50mb.db` — see the scenario's own comment: a genuinely correlated
per-row scan against the larger fixture multiplies past the 50M-step
VDBE guard rail before criterion can measure it, which is itself
evidence of the cost this ADR is about):

```sql
SELECT id, x FROM bench_data
WHERE bucket > (SELECT code FROM bench_lookup WHERE code = bench_data.bucket)
```

This is the same shape as #306's fixed scenario, except the inner
`WHERE` now references `bench_data.bucket` (the outer row), making it
genuinely correlated and therefore ineligible for #306's hoist. Run via
`make bench` / `cargo bench --bench engine -- correlated_subquery`.

## Consequences

- The `O(outer_rows)` re-materialization cost for correlated subqueries
  remains until the follow-up ticket lands; it is now benchmarked
  (previously only anecdotally described in module docs) and the fix
  shape is scoped, not open-ended.
- Follow-up ticket to file: "perf: memoize correlated scalar/`IN`
  subquery results keyed on outer-referenced column(s)" — single-table
  `WHERE`-clause scope only, building directly on #306's correlation
  walk and materialize/test split. `JOIN`-nested correlated subqueries
  and the full coroutine model both stay explicitly out of scope for
  that follow-up too, to be re-evaluated only if their own evidence
  warrants it.
