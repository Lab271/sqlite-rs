# 0034. Real-index range seeks for BETWEEN/IN/LIKE-prefix

Date: 2026-08-28

## Context

Every real-index opcode before #606 (`SeekIndexEq`, and `IdxRewind`/
`IdxLast`/`IdxNext`/`IdxPrev` for ORDER BY, #296/ADR-0020) is either an
exact-key point lookup or a full unbounded walk. `WHERE col BETWEEN lo
AND hi`, `WHERE col IN (...)`, and `WHERE col LIKE 'prefix%'` against an
indexed column all fall back to a full `Rewind`/`Next` table scan plus a
per-row `Ge`/`Le`, `Eq`-loop, or `like()` function-call filter
(`src/codegen/expr/cond.rs`) — O(n) instead of O(log n + k) for k
matching rows, even when an index on `col` exists.

## Decision

- Two new opcodes give the real index-read cursor (`IndexReadState` in
  `src/vdbe/cursor.rs`) a genuine bounded range walk: `SeekIndexGE`
  seeks to the first key `>=` a probe (no exact-match recheck, unlike
  `SeekIndexEq` — landing on any `>=` row is the correct range floor),
  and `IdxCompareGT` decodes the cursor's current leading columns and
  jumps once they exceed an upper-bound probe, becoming the stop check
  for an `IdxNext`-driven walk.
- `src/codegen/select/range_scan.rs` adds three narrow codegen fast
  paths (mirroring `limit_scan.rs`'s existing `try_compile_rowid_seek`
  style): `BETWEEN` and `LIKE`/`GLOB`-prefix both compile to
  `SeekIndexGE` + an `IdxCompareGT`-guarded `IdxNext` loop; `IN (...)`
  compiles to a sequence of `SeekIndexEq` point lookups (no new opcode
  needed there — it was already point-lookup shaped, just never looped
  over more than one value). Each path bails (`Ok(false)`) outside its
  recognized shape, falling back to the unchanged filter lowering in
  `cond.rs`.
- `LIKE 'prefix%'` reuses `SeekIndexGE`/`IdxCompareGT` rather than a
  dedicated byte-prefix-compare opcode: the upper bound is `prefix` with
  `char::MAX` (U+10FFFF) appended, which sorts above every string
  sharing that prefix without needing a raw non-UTF8 byte.

## Alternatives rejected

- **A general range/interval representation in `src/planner.rs`.**
  `planner.rs` today only holds join-order costing (`Stats`/`PlanCost`).
  A constraint IR general enough for arbitrary range predicates is far
  more machinery than three fixed shapes need, and every other fast
  path in this codebase (`try_compile_rowid_seek`,
  `try_compile_covering_index_scan`, `find_skip_scan_index`) is a narrow
  per-shape pattern match, not a general planner rule — consistent with
  that precedent over inventing a new abstraction layer.
- **A dedicated byte-prefix-compare opcode for `LIKE`.** Decoding a
  column and comparing only its first N bytes against a literal prefix
  is a plausible alternative to the `char::MAX`-appended-upper-bound
  trick, but it would need its own comparison semantics distinct from
  every other collation-aware `compare()` call in this codebase. Reusing
  `IdxCompareGT` keeps `LIKE`'s range walk mechanically identical to
  `BETWEEN`'s, at the cost of the `char::MAX` upper-bound needing this
  ADR to explain why it's correct.
- **SQLite's own byte-increment upper bound** (`'foo'` → exclusive bound
  `'fop'`). Correct in SQLite's byte-oriented world, but this crate's
  `Value::Text` is a Rust `String` (valid UTF-8 required) — incrementing
  a prefix's trailing byte can produce a byte sequence that isn't valid
  UTF-8 at all (e.g. a prefix ending on a multi-byte codepoint), so it
  can't always be represented as a `Value::Text`. Appending `char::MAX`
  keeps the bound a legal Rust string address for every ASCII/Unicode
  prefix this fast path accepts.

## Consequences

- `SeekIndexGE`/`IdxCompareGT` reopen the frozen V2 opcode set, per the
  same precedent as `IdxRewind`/`IdxLast`/`IdxNext`/`IdxPrev`
  (ADR-0020) and `SeekIndexEq`/`IdxRowid` (#243) — excluded from
  `Opcode::ALL` (never harvested from a V2 `EXPLAIN`) but fully
  dispatched and exhaustiveness-checked.
- The `char::MAX`-upper-bound trick has one acknowledged edge case: a
  row whose value is `prefix` followed immediately by `char::MAX`
  itself would be excluded even though it matches `prefix%`. This is
  the same class of edge case SQLite's own byte-increment trick has at
  the maximum byte value, and is bailed out of at the *lower* bound (a
  literal prefix ending in `char::MAX`) by `range_scan.rs`'s
  `like_literal_prefix`, but the *matched* value's own trailing
  character isn't and can't practically be excluded at seek time —
  accepted as out of scope, consistent with the issue's own bail-out
  list treating this class of boundary as acceptable risk.
- `IN (...)`'s per-value `SeekIndexEq` chain does not attempt to merge
  contiguous or overlapping values into a single range seek — each
  value is an independent point lookup. Fine for the common case (a
  short literal list), but an `IN` list with many contiguous integers
  gets no benefit over `BETWEEN`; revisit only if a real workload shows
  it matters.
- All three fast paths additionally require each literal operand's
  storage class to already match the indexed column's declared
  affinity (`range_scan.rs`'s `operand_matches_column_affinity`) before
  trusting it as a seek probe. A seek compares a raw probe key
  byte-for-byte against what's actually stored in the index (itself
  built with the column's affinity applied at `INSERT` time); the
  ordinary filter path's `Ge`/`Le`/`Eq` opcodes instead apply SQLite's
  *comparison* affinity coercion dynamically at compare time from both
  operands' runtime types. Reproducing that coercion (well-formed
  numeric text only, applied at compile time to a literal) was judged
  more machinery than three fixed shapes warrant; a mismatched operand
  (e.g. `'10'` against an `INTEGER`-affinity column) instead falls back
  to the ordinary scan, which already gets it right. `LIKE`/`GLOB`
  additionally requires `Affinity::Text` outright, since a prefix seek
  is meaningless against a column whose index entries are actually
  stored as coerced numbers.
