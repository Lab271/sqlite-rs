# Spike 012: Research Hash Aggregation for GROUP BY

**Issue:** #449

## Problem

Investigate whether hash aggregation would improve `group_by_agg` performance
(reported 27x slower than oracle). Current implementation is sort-then-group
(`src/codegen/select/aggregate.rs`), mirroring SQLite's own `select.c` shape.

## Baseline Results (2026-08-23)

`cargo bench --bench engine -- group_by_agg` (`SELECT bucket, COUNT(*), SUM(x)
FROM bench_data GROUP BY bucket`):

| Fixture | sqlite-rs | Oracle | Ratio |
|---------|----------:|-------:|------:|
| bench_1mb.db (16.7k rows) | 18.2 ms | 1.50 ms | 12.1x |
| bench_50mb.db (830k rows) | 1.096 s | 115.5 ms | 9.5x |

(Current ratio is ~10-12x, not the 27x cited in the issue — that number is
stale relative to the current codebase, or measured under different
conditions. Still a large, real gap.)

## Hypotheses

1. Sort overhead (`Sorter*` opcode family) dominates
2. Aggregate function call overhead (`AggStep`/`AggFinal`) dominates
3. Per-row allocation (record encode/decode, register churn) dominates

## Investigation

`cargo-flamegraph` on macOS requires `sudo` (dtrace) with no non-interactive
password available in this environment, so profiling instead used the
built-in macOS `sample` tool against the running bench process (no elevated
privileges required, same sampling-profiler technique):

```bash
source tools/bench_env.sh
./target/release/deps/engine-<hash> --bench "group_by_agg/bench_50mb.db/ours" --profile-time 15 &
sample <pid> 8 -f profile-group_by_agg-50mb.txt
```

Raw output (call graph + "top of stack" leaf histogram): `profile-group_by_agg-50mb.txt`.

## Findings

Leaf-frame histogram over 8s / ~6600 samples of the 50MB `group_by_agg`
bench, grouped by category:

| Category | Samples | Share | Symbols |
|----------|--------:|------:|---------|
| malloc/free/realloc (allocator) | ~2300 | ~35% | `_xzm_free`, `_xzm_xzone_malloc`, `_malloc_zone_realloc`, `_free`, `_realloc`, `rdl_alloc`/`rdl_realloc`, `RawVecInner::finish_grow`, `RawVecInner::reserve` |
| pager page-cache hashing | ~654 | ~10% | `hashbrown::raw::RawIterRange::fold` (backs the `HashMap<u32, Vec<u8>>` page caches in `src/pager.rs`, not GROUP BY) |
| record decode | ~730 | ~11% | `decode_column`, `decode_serial_value`, `decode_varint_at`, `decode_record`, `decode_text` |
| record encode (sorter spill) | ~270 | ~4% | `encode_record`, `make_record`, `serial_type_and_body`, `encode_varint` |
| VM dispatch loop | ~413 | ~6% | `vdbe::exec::run` |
| memmove/memset/bzero | ~650 | ~10% | `_platform_memmove`, `_platform_memset`, `__bzero` |
| **sort itself** | **~322** | **~5%** | `core::slice::sort::stable::merge::merge`, `::drift::sort` |
| register/compare/misc | ~450 | ~7% | `set_register`, `vdbe::compare::compare`, `apply_affinity`, `from_utf8` |

The call-graph detail (see raw file) shows the growth path concretely:
`make_record` → `encode_record` → `RawVecInner::reserve` →
`RawVecInner::finish_grow` → `_realloc`/`_malloc_zone_realloc` — i.e. every
row inserted into the sorter re-encodes a record into a freshly (re)grown
`Vec<u8>`, which is a large fraction of the allocator time.

**The sort algorithm itself is a small fraction (~5%) of total time.** The
dominant costs are allocator churn from per-row record
encode/decode/materialization and pager page-cache bookkeeping — none of
which a hash table would avoid. A hash-aggregation path still needs to
decode every row's group-by key and value, and still needs to store/update
one accumulator entry per group (itself a hash lookup + possible
allocation), so it inherits the same decode-side costs and trades the
sort's ~5% for hash-bucket lookup/rehash overhead instead.

## Recommendation

**Skip hash aggregation for now.** It targets the wrong bottleneck: sorting
is not where `group_by_agg`'s time goes. The real wins are:

1. Avoid re-encoding/re-allocating a full record per row when spilling into
   the sorter (`make_record`/`encode_record` growth path) — e.g. reuse a
   scratch buffer across `SorterInsert` calls instead of allocating fresh.
2. Reduce record-decode overhead on the read side (per-column varint/serial
   decoding is called extremely often).
3. Pager page-cache (`HashMap<u32, Vec<u8>>`) lookup overhead scales with
   pages touched during the sort's read/spill passes; worth profiling
   separately from GROUP BY specifically.

These are general VDBE/record/pager performance items, not GROUP BY-specific,
and are better tracked as their own follow-up tickets/spikes than folded into
a hash-aggregation implementation that wouldn't move the needle. No ADR is
proposed since no architectural change is being made (sort-based grouping is
retained).
