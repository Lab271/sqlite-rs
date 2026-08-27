# 0032 — Hash `GROUP BY` is a second strategy with its own opcode family, and still emits groups in key order

**Status:** Accepted · **Date:** 2026-08-27

## Context

`GROUP BY` (spec 009 Requirement 12, #239/#263) executes by sorting every
WHERE-matching row by its group key and then walking the sorted stream
detecting key changes — O(n log n) in the row count, paid entirely to
make a group's rows *adjacent* so one accumulator per aggregate can be
reused across the group and reset at each boundary. A hash table needs no
adjacency at all: it folds each row into its own group's accumulators as
the scan reaches it, O(n). #570 asked for that, with the sort strategy
retained (spec 001 Tier 3 — "simplifiable, not droppable").

Three questions had to be settled before writing any of it.

## Decision

**1. A new opcode family, not a reuse of `OpenEphemeral`.** Requirement
12 originally noted that `OpenEphemeral`'s in-memory ephemeral table is
"reused as the GROUP BY grouping-table backing store — no new cursor
machinery". That store is a rowid-keyed row list; it can hold rows but
cannot fold accumulators into them, which is the entire point. Hash
aggregation gets `HashAggOpen`/`HashAggFind`/`HashAggStep`/
`HashAggRewind`/`HashAggData`/`HashAggNext` and a `CursorSlot::HashAgg`,
deliberately shaped one-for-one after the `Sorter*` family so a reader
can diff the two strategies opcode by opcode.

What is *not* duplicated: `HashAggStep` calls the same
`src/vdbe/aggregate.rs` `step` kernel `AggStep` does, and `HashAggData`
installs its group's accumulators into the ordinary `AggStep`/`AggFinal`
context slots — so the per-group flush codegen (`flush_group`: `HAVING`,
`LIMIT`, projection, `AggFinal`) is shared verbatim, not reimplemented.
An aggregate cannot mean one thing under each strategy because there is
only one implementation of what it means.

**2. Groups are emitted in group-key order, not hash or insertion
order.** SQLite guarantees no `GROUP BY` output order, so hash order
would be *permissible*. It was rejected anyway: the sort strategy's
order is key order, and every oracle diff and fixture in this repo is
written against it, so shipping hash order would turn a performance
change into a broad, silent output change. `HashAggRewind` therefore
sorts the K groups before iterating. This costs O(K log K) — over the
*groups*, not the O(n log n) over the *rows* it replaces — so the
asymptotic win survives for exactly the low/medium-cardinality queries
this path targets, and the output stays byte-identical to before.

**3. Group identity is a canonical key encoding, not a hash of
`Value`.** `Value` has no `Hash`/`Eq` and must not get one for this:
SQLite merges INTEGER and REAL into one numeric class (`1` and `1.0` are
one group), text equality depends on the column's collation, and the
column's comparison affinity applies first. `src/vdbe/hash_agg.rs`
canonicalizes each key value — affinity applied, collation folded,
integral REALs encoded as the integer they name — so that two values
share a bucket exactly when Requirement 5's `compare` calls them equal.
That is the strategy's single load-bearing invariant, and it is what the
oracle diffs in `tests/corpus/hash_group_by_test.rs` exist to hold.

## Alternatives rejected

- **Replace the sort strategy.** Rejected by the tier model, and by
  `DISTINCT` aggregates, whose per-group dedup set is reopened at each
  group *boundary* — a construct that only exists when rows are
  adjacent. Those fall back to the sorter.
- **Hash-bucket the rows, then keep the existing boundary-detection
  pass.** Would have needed no codegen changes at all (bucketing merely
  makes a group's rows adjacent, and the existing `Eq` chain then
  re-verifies each boundary — a useful safety net against an
  over-coarse key encoding). Rejected because it still buffers and
  re-decodes every row, which is most of the cost the ticket is about;
  the `Eq` safety net is replaced by direct unit tests on the key
  encoding.
- **Derive `Hash`/`Eq` on `Value`.** Rejected: `f64` blocks `Eq`, and
  any such instance would encode BINARY-collation, affinity-free
  equality — silently wrong for exactly the cases this feature must get
  right.

## Consequences

- Two `GROUP BY` strategies now exist behind one dispatch point
  (`compile_select_scan`), three counting the index-ordered walk (#310).
  Each new `GROUP BY` feature must decide which strategies it works
  under, and say so in the hash path's documented narrowings.
- The key encoding is a second, independent definition of "equal"
  alongside `compare`. It is unit-tested against `compare`'s cases
  directly, but a future change to `compare` (a new collation, a
  numeric-comparison fix) must update both or the strategies will
  silently disagree.
- Measured on a 1.5M-row, 20-group scan: 0.83s (sort) → 0.62s (hash),
  against 0.23s for the pinned oracle. The remaining gap is per-row scan
  and `MakeRecord` cost, not grouping — the hash path still builds a
  record for every row when only one row per *group* is ever retained,
  which is the next thing to attack.
