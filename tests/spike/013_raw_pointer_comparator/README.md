# Spike 013: Does sqlite3's Raw-Pointer Comparator Trick Actually Explain the Gap?

**Issue:** #631 (follow-up)

## Problem

PR #632 closed #631's `group_by_agg` gap from 4.12x to ~2.6-3.6x vs the
pinned oracle, but couldn't close it further under
`unsafe_code = "deny"`: reading sqlite3's real source
(`vdbesort.c::vdbeSorterCompareInt`) showed its sort-key comparator uses raw
pointer arithmetic and zero bounds checks — a `memcmp`-style byte compare
with no `Value` construction at all. The obvious hypothesis: **the
remaining gap is the cost of Rust's safety** (bounds checks on every byte
access), and matching sqlite3 would require `unsafe`.

This spike tests that hypothesis directly instead of assuming it.

## Design

Three comparators (`src/lib.rs`), same minimal single-column INTEGER record
format, same test data (bucket-like values: small, mostly non-negative,
matching #631's actual `bench_data.bucket` benchmark column):

1. **`regular::compare`** — the *general* decode pipeline sqlite-rs used
   for every sorter key column before #631: parse the whole header into a
   freshly allocated `Vec<(serial_type, offset)>`, decode into a tagged
   `Value` enum via a multi-arm `match` (covering every serial type the
   real record format defines, not just integers), compare via a
   generic, type-ranking dispatcher. Safe Rust throughout — mirrors
   `src/record/decode.rs`/`src/vdbe/compare.rs`'s actual shape.
2. **`safe_fast::compare`** — the algorithm sqlite-rs actually shipped in
   #631 (`decode_single_column` + a same-serial-type byte compare): no
   header `Vec`, no `Value` enum, direct byte comparison on same-width
   integers. Every access is bounds-checked (`.get()`). Still 100% safe
   Rust.
3. **`unsafe_trick::compare`** — a literal port of sqlite3's
   `vdbeSorterCompareInt`: raw pointer arithmetic (`ptr.add(i)`,
   `*ptr`), zero bounds checks, wrapped in `unsafe fn`.

All three are checked for agreement (`cargo test`) before any timing is
trusted — see `all_three_comparators_agree_on_every_pair`.

## Results (`cargo bench`, Apple Silicon, release)

**Single comparison** (`compare_one_pair`):

| Comparator | Time | vs `regular` | vs `safe_fast` |
|---|---:|---:|---:|
| `regular` | 36.4 ns | 1x | — |
| `safe_fast` | 1.48 ns | **24.6x faster** | 1x |
| `unsafe_trick` | 1.45 ns | 25.1x faster | **1.02x faster** |

**Sorting 50,000 rows** (`sort_50000_rows`, the realistic shape —
`Vec::sort_by` calling the comparator ~n·log(n) times):

| Comparator | Time | vs `regular` | vs `safe_fast` |
|---|---:|---:|---:|
| `regular` | 21.84 ms | 1x | — |
| `safe_fast` | 2.90 ms | **7.5x faster** | 1x |
| `unsafe_trick` | 2.84 ms | 7.7x faster | **1.02x faster** |

## Finding

**The raw-pointer trick itself buys ~2%. Avoiding the general decode
pipeline buys 7.5-24.6x.** Bounds checking was never the bottleneck —
`safe_fast` and `unsafe_trick` are within noise of each other in both
benchmarks. The entire gap between `regular` and the other two is
architectural: a `Vec` allocation per comparison, a tagged `Value` enum
covering every serial type the format defines (not just the ones present),
and a generic collation-aware dispatcher — none of which the specialized
path needs when it already knows "both sides are the same-width integer."

This directly contradicts the natural assumption ("sqlite3 uses unsafe, so
that's why it's fast"). sqlite3's *actual* edge here is that it special-cases
common shapes at all (`vdbeSorterCompareInt`/`vdbeSorterCompareText` vs the
general `sqlite3VdbeRecordCompare`) — the raw pointers are a separate,
much smaller optimization on top of that specialization, not the source of
it.

## Conclusion

#631/PR #632's remaining ~2.6-3.6x gap to the oracle is **not** explained by
Rust's safety costs and is **not** blocked by `unsafe_code = "deny"`. The
`decode_single_column` fast path PR #632 already shipped captures
essentially all of the algorithmic win available from this specific
optimization (specialization over generality) — a hypothetical `unsafe`
version of it would buy at most ~2% more, not another multiple.

**Where the remaining gap actually is**, per PR #632's own profiling, is
elsewhere in the pipeline: `vdbe::exec::run`'s general opcode-dispatch loop
(~97% of wall time is *inside* it, though that includes everything else
too), `MakeRecord`'s encode path, and the pager/page-cache layer noted in
spike 012. None of those are specific to comparators, and none of them are
"turn on `unsafe`" fixes either, per this spike's evidence — they'd need
their own equivalent "does specialization beat safety-cost avoidance"
investigation before assuming the fix is unsafe code.

No ADR proposed: no production code changed by this spike, and it argues
*against* reaching for `unsafe` here, which needs no architectural decision
to not do.
