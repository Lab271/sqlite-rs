# 0027: Correlated scalar subquery equality compiles to a seek, not a cache lookup

Date: 2026-08-23
Status: Accepted

## Context

#434 reported a correlated scalar subquery still 785x slower than the
sqlite3 oracle:

```sql
SELECT id, x FROM bench_data
WHERE bucket > (SELECT code FROM bench_lookup WHERE code = bench_data.bucket)
```

ADR-0021 scoped a memoization cache (#314,
`src/codegen/subquery/memoize.rs`) for exactly this correlated-subquery
shape, keyed on the outer-referenced column. That cache is real and
correct, but caps at 8 distinct probe values
(`MAX_MEMO_CACHE_ENTRIES`) — a deliberate bound, since the cache is a
linear-scan ephemeral table and an unbounded cap would itself blow the
VDBE step guard rail on a large outer scan. `bench_data.bucket` in the
#301 bench fixture has 1000 distinct values, so the cache fills after 8
and falls back to uncached re-execution for the rest — #434's residual
785x.

Comparing against the sqlite3 oracle's own `EXPLAIN` for this exact
query revealed it does not cache anything at all. It re-runs the
subquery as a subroutine on every outer row (`BeginSubrtn`/`Return`),
but each run is cheap because `lookup.cat = t.y` — an equality against
`lookup`'s `INTEGER PRIMARY KEY` — compiles to a single `SeekRowid`
point lookup instead of a full-table scan. There is no oracle-side
caching to replicate; the actual gap was that
`compile_scalar_subquery` (`src/codegen/subquery/scalar.rs`) always
compiled its `WHERE` clause as an unconditional `Rewind`/`Next` scan,
regardless of whether that `WHERE` clause was a trivially seekable
equality.

This codebase already has the exact access-strategy classifier needed
for this: `src/codegen/select/join_access.rs`'s `choose_join_access`,
built for #243 to turn a `JOIN ... ON` equality against a rowid or
`UNIQUE`-indexed column into a `SeekRowid`/`SeekIndexEq` point lookup
instead of a nested-loop scan.

## Decision

**Reuse `join_access::choose_join_access` inside
`compile_scalar_subquery`**, treating the subquery's own `WHERE`
clause the same way a `JOIN ... ON` clause is treated: if it is a
single top-level equality between the subquery's table (rowid or a
single-column `UNIQUE` index) and a safe reference to an
already-bound outer table, compile a `SeekRowid` (or
`SeekIndexEq`+`IdxRowid`+`SeekRowid`) point lookup instead of the
`Rewind`/`Next` scan. A `NULL` probe value is checked explicitly
(`IsNull`) before either seek opcode, since `SeekRowid` requires an
integer key and SQL's `NULL = x` is always unknown, matching the
existing scan path's null-handling.

This required elevating `JoinAccess`/`choose_join_access` from
`pub(super)` (visible only within `codegen::select`) to
`pub(in crate::codegen)` so `codegen::subquery` can reach them; no
other visibility change was needed since `TableBinding` was already
`pub(crate)`.

`join_access::choose_join_access` and the required
`crate::codegen::index_maintenance::valid_index_root_page` are both
existing, already-tested primitives — this is a reuse, not a new
mechanism. No new VDBE opcode was needed or added.

#314's memoization cache (ADR-0021) is left entirely in place: this
seek fast path is checked first, and the cache still applies to a
correlated subquery whose `WHERE` clause is *not* a seekable equality
(a range comparison, or an equality on a non-indexed column) — the two
optimizations are complementary, not overlapping.

## Alternatives rejected

- **Raise `MAX_MEMO_CACHE_ENTRIES` and/or make the memo cache O(log n)
  instead of O(cap) linear** (e.g. an index-mode ephemeral cursor with
  a new opcode to read back a cached payload column, since the
  existing `Column` opcode explicitly rejects index-mode ephemeral
  cursors and `IdxInsert`'s key is always the full inserted record).
  Rejected: this would have required inventing a VDBE opcode with no
  real sqlite3 counterpart (confirmed by diffing against the pinned
  oracle's own opcode set), just to reimplement a cache the oracle
  itself doesn't use for this shape. The seek-based fix instead matches
  the oracle's actual technique, uses only opcodes that already exist
  in real SQLite, and fixes the case unconditionally rather than only
  below some cardinality cap.
- **Extend the seek fast path to `EXISTS`/`IN` subqueries in the same
  PR**: deferred — #434's own scenario is a scalar subquery; `EXISTS`
  (`compile_exists`) and `IN` (`compile_in_subquery_multi`) have
  separate compilation paths that would need their own (likely
  similar) integration, better scoped as its own follow-up once this
  shape is proven out.

## Consequences

- `correlated_subquery` in `tests/performance/engine.rs` (#303/#434)
  goes from 785x oracle-relative and unmeasurable against
  `bench_50mb.db` (blew the 50M-step VDBE guard rail) to ~14-15x on
  both fixtures — the scenario's `bench_50mb.db` skip is removed.
- A correlated subquery equality against a non-indexed, non-rowid
  column still falls back to #314's cache (if the outer column is
  low-cardinality) or an uncached scan (otherwise) — genuinely
  unindexed correlated lookups remain a follow-up if a real workload
  ever demonstrates the need.
- `EXISTS`/`IN` subqueries with an equivalent seekable equality are not
  yet covered by this fast path — a targeted follow-up ticket, not
  urgent absent benchmark evidence.
