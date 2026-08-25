# Changelog

All notable changes to sqlite-rs. Format follows [Keep a Changelog](https://keepachangelog.com/), versioning follows [SemVer](https://semver.org/). Pre-1.0: minor bumps may break the public API.

**Versioning policy:** one minor version per completed plan phase — the version number tells the plan's story, sub-steps stay inside a phase. V1 (READ CORE) = 0.1.0 through 0.4.0. *(History note: internal iterations briefly numbered 0.4.0–0.6.0 were renumbered into the phase scheme on 14 Aug 2026, before any tag or publication of those versions existed.)*

## [Unreleased]

### Added

- 9 read-only introspection `PRAGMA`s (#489): `table_info`,
`table_list`, `index_list`, `index_info`, `database_list`,
`schema_version`, `user_version`, `page_size`, `page_count`. Recognized
by a hand-rolled parser (`src/bin/sqlite-rs/pragma_query.rs`)
deliberately outside the main grammar/AST/codegen/VDBE pipeline —
these are synthetic in-memory result sets built directly from
already-loaded schema/header data (the `EXPLAIN QUERY PLAN` precedent,
not the `journal_mode` write-pragma path), so they never touch a
`Pager` transaction or compile to bytecode. Wired into both the
`query` subcommand and `repl`; a `PRAGMA` outside these 9 names (e.g.
`journal_mode`) falls through to existing behavior unchanged.
`schema::column_defs`/`column_type` (`src/schema/ddl_reader.rs`) are
now `pub` (were `pub(crate)`) so the CLI-layer pragma module can reuse
them instead of re-deriving column-definition splitting. Scope cuts:
`index_list`/`index_info` only report explicit `CREATE INDEX` entries
(`origin` is always `c`) — auto-indexes for inline `PRIMARY
KEY`/`UNIQUE` constraints are already dropped by
`schema::read_schema` and not re-derived here; `table_list` omits the
internal `sqlite_schema`/`sqlite_temp_schema` rows stock `sqlite3`
lists (no temp-db support). Tests: `tests/unit/introspection_pragmas.rs`.

## [0.18.2] - 2026-08-25

### Fixed

- CLI `exec` failed to bootstrap a brand-new database file (#448):
  `sqlite-rs exec <file> "<sql>"` required the target file to already
  have a valid SQLite header, unlike stock `sqlite3 <file> "<sql>"`
  which creates the file lazily on first write. Added
  `DatabaseHeader::new_empty_page1` (`src/header.rs`) to build a valid
  empty-database page 1, written by `run_exec`
  (`src/bin/sqlite-rs/exec.rs`) before opening whenever the target path
  doesn't exist yet.

## [0.18.1] - 2026-08-25

### Added

- track and apply column-declared `COLLATE` across schema and
comparisons (#500) — `TableSchema`/`IndexedColumn`
(`src/schema/ddl_reader.rs`) now capture each column's/index-column's
declared `COLLATE` (default `Binary`), previously parsed and
discarded. A new `expr_collation()` (`src/codegen/expr/value.rs`) falls
back to a bare column's declared collation whenever the query has no
explicit `COLLATE`, wired into WHERE/IN comparisons, `ORDER BY`,
`GROUP BY`, and `min`/`max` aggregate comparisons — an explicit
query-side `COLLATE` still wins. `SeekIndexEq`'s probe and the
#450/#492 duplicate-key recheck now carry the leading index column's
collation via a new `P4::SeekKey` payload instead of hardcoding
`Binary`. `Collation`/`compare_text` moved from `src/vdbe` to
`src/record` to keep `schema`'s Tier 0 layer isolation intact. `SELECT
DISTINCT`'s ephemeral-index dedup (byte-equality on encoded records,
never calling `compare()`) is a separate mechanism and was filed as a
follow-up (#518) rather than folded in here.

### Fixed

- `SELECT DISTINCT` respects declared/explicit `COLLATE` (#518) —
the ephemeral-index dedup path (`Found`/`IdxInsert` against an
in-memory `BTreeMap`) compared raw encoded record bytes, ignoring
collation entirely; a `COLLATE NOCASE` column returned case-variant
duplicates as distinct rows. Codegen now resolves each result column's
collation (mirroring #500's `resolve_order_by` fallback) into a new
`P4::SeekKey` operand, and the ephemeral-cursor key-building
normalizes `NoCase`/`RTrim` text before encoding so byte-equality on
the normalized key matches `compare()`'s notion of equality. UNION's
shared dedup path stays `Binary`-only, matching its existing
conservative ORDER BY handling.

- assurance's `plan_blocks()` regex dropped V5/V6 ("V5 Slim"/"V6
Slim" in plan.md's table didn't match the bare-tag-only regex),
causing a false "grammar tags not in plan.md value blocks" drift
report; also renamed `tests/parity/v04.rs`-`v07.rs`'s test fns to name
the dimensions they actually cover (`acceptance`/`output`), fixing
`make assurance-gate`'s parity count from a misleading 3/12 to the
accurate 7/12 — no test logic changed, only a naming-convention gap
that made real coverage invisible to the dashboard's name-based
heuristic.

- JOIN reordering now prefers a rowid/unique-index-seekable inner
table over raw ANALYZE row-count ordering (#510) —
`join_order::seekable_tables` flags a table whose `ON` equality is a
structural rowid-alias/single-column-`UNIQUE`-index match (the same
shape `join_access::choose_join_access` looks for), and `scan_costs`
gives such a table `u64::MAX` so `plan_join_order`'s ascending sort
always places it innermost, letting the existing seek codegen fire
regardless of the table's own size — a rowid/index seek is O(1)/O(log
n) and always cheaper as an inner probe than as the outer scan. Fixes
the `bench_data JOIN bench_lookup ON bench_data.bucket =
bench_lookup.code` case (`bench_lookup.code` an `INTEGER PRIMARY KEY`)
where the smaller table's row count previously won it the outer scan
slot, forcing a full scan on the larger table's join column instead of
a `SeekRowid` on the smaller one. spend: matched estimate.

- unindexed `GROUP BY` aggregate sort-pipeline overhead (#506) —
`compile_grouped_scan`'s pass 1 now only serializes columns actually
referenced by the `GROUP BY` key, aggregate arguments, or plain
result/`HAVING` columns (every other schema column becomes a cheap
`Null` placeholder rather than a real per-row read); `SorterInsert` no
longer copies the record blob on every insert (reuses the already-
`Rc`'d bytes); `OpenPseudo` is now emitted once before pass 2's loop
instead of once per row. Also fixed a pre-existing, ticket-adjacent bug
found while adding regression coverage: a plain (non-key,
non-aggregate) result/`HAVING` column's "arbitrary row" snapshot picked
the group's *last* row instead of the *first*, mismatching the real
oracle's own sort-then-group behavior. `group_by_agg` benchmark:
20.8ms -> 11.2ms on the 1MB fixture (~46% faster), still short of the
ticket's `<3x`-oracle target — the residual gap looks architectural
(VDBE per-instruction dispatch, the sort pipeline's inherent
double-decode), out of this ticket's scope. spend: ~2-3x estimate.

### Performance

- trim `run()`'s per-instruction dispatch overhead (#509) —
`checked_add`+`.ok_or` on the step counter and the program-counter
increment are replaced with `saturating_add` (both are backstops
against a pathological program, not values any real program comes
close to overflowing, so the `Option` construction/unwrap on every
single instruction was pure overhead), and `program.get(pc).ok_or(...)`
is replaced with a `let-else` on the same `Option` to skip the extra
error-value construction on the hot path. Confirmed via a stashed
before/after `cargo bench --bench engine` A/B on the 50MB fixture
(criterion `--save-baseline`/`--baseline`, not just cross-run deltas,
since the oracle's own unchanged-code runs showed up to ~2.5% run-to-
run noise on this machine): `full_scan` -2.8%, `full_scan_1col` -4.3%
(both p<0.05, above the noise floor), `full_scan_3col` -2.1% (within
noise). A batched, check-every-4096-steps variant of the step-limit
comparison was also tried and measured no further win beyond the above
(ours/oracle ratio unchanged within noise), so it was dropped rather
than kept as unjustified complexity, per the ticket's own evaluate-and-
keep-or-drop mandate. Candidate #1 (streaming rows instead of
materializing `vm.rows: Vec<Vec<Value>>`) was evaluated and deferred:
every current caller (CLI `query`/`repl`, the write-path executor)
consumes rows fully after the fact, so streaming would mean threading
a new execution API through every call site for what both this issue's
own data and #506's prior finding indicate is a modest, not
gap-closing, win — filed as its own follow-up rather than attempted
speculatively here. Candidate #3 (specializing `Next`/`Rewind` so a
tight scan loop skips re-entering `dispatch()`'s generic match) doesn't
have a safe, small-scope implementation: Rust already compiles
`dispatch()`'s opcode match to a jump table, so there's no per-arm
match-order cost to cut, and a real fast path would mean recognizing
and specially executing a whole loop body between `Rewind`/`Next` and
its matching jump — a genuine VM-architecture change (superinstructions
/ threaded code), not a contained edit; filed as a separate, better-
scoped follow-up (#515) with today's benchmark numbers as its starting
evidence rather than attempted here. Candidate #4 (Column-opcode-
specific flat tax) is answered by the above: `full_scan_1col`/
`full_scan_3col` still sit at ~2.0x/~1.6x oracle after this fix,
confirming the residual gap is real column-decode cost, not a separate
per-call tax — matching #506's own "VDBE per-instruction dispatch...
architectural" note. `full_scan_1col`/`full_scan_3col`'s own remaining
ResultRow-side gap is being addressed separately (agent-3, not part of
this ticket). spend: matched estimate.

- `SorterInsert` now decodes only through the `ORDER BY` key's
highest column index instead of every selected column (#507) —
`SorterState` computes `decode_upto` (one past the max
`SortKeyColumn.index`) once at `SorterOpen`, and a new
`decode_record_upto` (`src/record/decode.rs`, reusing `decode_column`'s
header-walk-then-partial-decode pattern) replaces the prior full
`decode_record` call on every candidate row. #506 had already fixed the
double-copy half of this pattern (`Rc<[u8]>` reuse instead of
`blob.to_vec()`); this closes the remaining full-row-decode half for
the general-purpose sorter backing plain `ORDER BY ... LIMIT` (as
opposed to #506's `GROUP BY`-specific codegen path). `order_by_limit`
benchmark ratio vs the pinned oracle: ~3.2x-6.15x -> ~1.2x (37µs/30µs
on the 1MB fixture, 43.5µs/36.4µs on 50MB) — beats the ticket's
`<2x`-oracle target.
spend: matched estimate (medium)

- `TableCursor::seek` (backing the `SeekRowid` opcode) now binary
searches a page's cell-pointer array instead of scanning it linearly,
on both leaf pages (rowid comparison) and interior pages (separator-key
comparison) (#508). Repeated seeks against the same table — the `join`
tier-1 benchmark's dominant cost, one `SeekRowid` per outer row — no
longer pay an O(cells-per-page) decode-and-compare loop per call; the
`join` benchmark's ratio against the pinned oracle dropped from ~7.4x
to ~2.3x (14.07ms→4.5ms on the 1MB fixture, 1.16s→284ms on 50MB). This
was misattributed for a time to ADR-0022's missing-`Pager`-page-cache
gap, which had already been closed by #320/#457/#459 — see ADR-0028,
which supersedes ADR-0022's now-stale problem statement. ANALYZE and
join-ordering/access-selection were independently confirmed unaffected
(`EXPLAIN QUERY PLAN` is identical before and after, on both oracle and
ours: `SCAN bench_data` / `SEARCH bench_lookup USING INTEGER PRIMARY
KEY`) — the gap was purely in `seek`'s own per-page search algorithm,
not query planning.
spend: matched estimate (medium)

## [0.18.0] - 2026-08-25 — V7 Performance & Planner

Epic #421's V7.2 phase (Performance & Planner), now complete: the query
planner (`ANALYZE` + cost model, join ordering, Bloom-filter join
elimination, skip-scan), a run of targeted VDBE/pager performance work
(row-header cache, zero-copy payload, page-cache hashing, correlated-
subquery memoization, CTE materialization sharing), and the `/review`
follow-up that closed out its warning-level findings. V7.3 (PRAGMAs &
Introspection) is next.

docs: address V7.2 review warnings from epic #421 (#501) — verified and
documented, rather than patched, three warning-level findings that
were already structurally safe or a pre-existing crate-wide gap: (1)
`take_register` reuse in `ResultRow` is safe because `RegAlloc` never
reuses a register number and `GROUP BY`'s `prev_key` bookkeeping
registers are kept separate from anything `ResultRow` projects; (2)
`SeekIndexEq`'s duplicate-key recheck hardcodes `Collation::Binary`
consistently with the seek it walks past — no column-declared
`COLLATE` exists anywhere in `TableSchema` yet, so patching just the
recheck would have been inconsistent (tracked as the real fix in
#500); (3) CTE materialization cache reuse is safe today because no
volatile expression (`random()`, `CURRENT_TIME`, etc.) exists in the
parser yet — corrected an overstated "always guaranteed" doc comment
and spelled out what the first volatile function must account for.

fix: fail closed, not open, when a checkpoint's page-count bound
overflows `u32` — surfaced by a `make silent-swallow` robustness audit
(unrelated to epic #421). `checkpoint_passive`'s own comment states the
intent plainly: a corrupted-but-checksum-valid WAL frame with a
`page_num` near `u32::MAX` must never drive `write_at` to an arbitrary
offset beyond the database's actual extent — but the bound computing
the main file's current page count fell back to `u32::MAX` on `u32::
try_from` overflow, which made `max_page` effectively unbounded,
defeating the exact check the comment describes. Extracted into a
testable `page_count_from_size()` helper, now falling back to `0`
(`max_page` then falls back to the WAL's own already-validated
`db_size` bound) instead.

fix: support `GROUP BY`/aggregate combined with `ORDER BY` and a JOIN
(#502, found via the V07 parity suite #72) — `compile_joined_grouped_
scan` previously rejected this combination outright. A third codegen
pass now inserts each finalized group row into a second sorter keyed
by the resolved `ORDER BY` targets instead of sinking it directly;
`LIMIT`/`OFFSET` move from per-group-flush time to after that final
sort, since which rows a `LIMIT` keeps isn't known until the `ORDER
BY` order is resolved. `ORDER BY` terms resolve against a whole
aggregate call (matched structurally, not by `Expr` equality, since an
`ORDER BY` term is a separately-parsed AST node from its SELECT-list
twin), an ordinal, a result-column alias, or a bare joined column.
`DISTINCT` + JOIN + `GROUP BY` stays unsupported (split out of this
ticket's scope).

feat: share one materialization across repeated `FROM`-subquery
references (#425, epic #354's V6.1 "10x on repeated subqueries"
target) — `expand_with_clause` rewrote every reference to a
`WITH`-clause CTE into its own independent `TableRefKind::Subquery`,
so a CTE referenced N times re-ran and re-materialized its body N
times, same cost as inlining it N times. A new VDBE opcode,
`OpenDup(p1=new_cursor, p2=source_cursor)`, opens a second,
independently-scanning cursor sharing an already-materialized
ephemeral table's row data instead of a fresh `OpenEphemeral`+
populate; `RegAlloc` caches materializations per statement compile,
keyed by structural equality of the `Select` being materialized (not
an AST identity field — that approach grew `Select`'s inline size
enough to trip an unrelated pre-existing depth-guard test sitting at
the edge of a real stack overflow in debug builds). `cte_reuse_10x`
bench: `cte` 1.34ms vs `inline` 11.1ms (was statistically identical
before this fix) — roughly 8x.

chore: add WAL-mode variants to `engine.rs`'s transaction benchmarks
(#436) — `insert_single_tx_wal`, `insert_batch_tx_100_wal`,
`insert_batch_tx_1000_wal`, `update_batch_tx_wal` now run each
transaction scenario under `journal_mode=WAL` (via a `PRAGMA
journal_mode=WAL` switch excluded from the timed closure, mirroring
`v6.rs`'s existing `switch_to_wal` pattern) against the oracle,
alongside the existing DELETE-mode variants — previously only `v6.rs`
compared WAL vs DELETE, with no oracle reference point.

fix: share one `-shm` fd per path per process across all WAL lock
guards (#491, follow-up from #412's investigation) —
`WalWriteLock`/`WalCheckpointLock`/`WalReadLock`/`UnixWalShm`
(`src/vfs/shm.rs`) each opened an independent `File`, but POSIX
`fcntl` record locks are scoped to `(process, inode)`, not to a file
descriptor: closing any fd this process holds to a file releases every
lock the process holds on that inode, even ones taken through a
different, still-live fd. Two such guards held concurrently in one
process (e.g. `checkpoint::checkpoint_passive` called directly while a
separate `Pager` holds its own long-lived `WalReadLock` on the same
file) could have one guard's `Drop` silently release the other's still-
needed lock. A new `open_shm_shared` registry (`Arc`/`Weak`-backed,
keyed by path) makes every guard/helper reuse the one fd already open
for a path — it only actually closes once nothing in the process needs
a lock on that inode anymore. Derived from stock sqlite3's own
`os_unix.c`, whose `unixClose` defers closing for exactly this reason.

perf: index-mode memoization cache for correlated scalar subqueries
(#494, follow-up from #434/#435) — #314's per-probe-value cache
(`src/codegen/subquery/memoize.rs`) previously used a table-mode
`OpenEphemeral` cursor with a linear `Rewind`/`Eq`/`Next` scan per
lookup, capped at `MAX_MEMO_CACHE_ENTRIES = 8` distinct probe values to
bound worst-case VDBE step counts — any higher-cardinality correlated
column fell back to recomputing every row. The cache now uses an
index-mode `OpenEphemeral` cursor (`Found`/`IdxInsert`), backed by a
`BTreeMap` for an O(log n) lookup regardless of cache size, so the cap
is removed entirely (bounded only by the ephemeral cursor's existing
`MAX_EPHEMERAL_ROWS` ceiling). `IdxInsert` gains a `P5` operand (extra
payload-only registers beyond the `P4` key count) and `Column` gains
read support for index-mode ephemeral cursors, so a cache entry's key
(the probe value) and its cached result no longer have to be the same
registers (`src/vdbe/cursor.rs`).

feat(planner): skip-scan for non-leading composite-index columns (#485)
— `WHERE b = ?` against a composite index `(a, b)` (leading column `a`
unconstrained) now uses the index instead of a full table scan,
whenever `a`'s `ANALYZE`-derived `avg_eq` clears the oracle-confirmed
skip-scan threshold (empirically measured against sqlite3 3.51.0: `avg_eq
>= 18`, matching sqlite.org/optoverview.html's documented "~18
duplicates"). `is_skip_scan_worthwhile` (`src/planner.rs`) mirrors
oracle's own decision; `try_compile_skip_scan_index`
(`src/codegen/select/limit_scan.rs`) walks the whole index
(`IdxRewind`/`IdxNext`), checking the constrained column on each
narrower index entry and `IdxRowid`+`SeekRowid`-ing into the table only
for a match; `eqp.rs` reports the same oracle-verbatim EQP text
(`SEARCH t USING INDEX idx (ANY(category) AND price=?)`). Unlike real
SQLite's skip-scan (a genuine per-distinct-leading-value binary seek),
`IndexCursor::seek` in this codebase is a documented Tier 0 linear scan
— this walks every index entry rather than truly skipping past a large
group, so the measured win (`tests/performance/skip_scan.rs`, `make
bench-skip-scan`: ~1.24x, 10.04ms → 8.12ms at 200K rows/3 leading
values) comes from narrower index-row decode and selective table
lookups, not sub-linear seeking — reported honestly rather than as an
oracle-parity ratio. spend: ~1.2M token budget, matched estimate.

perf: zero-copy `IndexRow` payload for index/WITHOUT ROWID scans (#471)
— follow-up to #467, which left `IndexRow::payload` (src/btree/index.rs)
as an owned `Vec<u8>` out of scope. `IndexFrame.page` is now `Rc<[u8]>`
(matching `PageSource::read_page`'s return type); `IndexRow::payload`
reuses the `Payload` enum #467 introduced instead of `Vec<u8>`;
`decode_leaf_entry`/`decode_interior_entry` pass `&frame.page` directly
into `reassemble_payload` instead of wrapping it in a throwaway `Rc`
and immediately copying the result back to a `Vec`. `decode_value_cell`
(the Pager-only index insert/delete write path) is unchanged.

perf: non-`UNIQUE`-index duplicate-key matches for the covering-index
scan and index-only `COUNT(*)` fast paths (#450, follow-up from #444)
— both fast paths previously required a `UNIQUE` index because
`SeekIndexEq`'s one-shot probe couldn't walk forward past duplicate
keys. `SeekIndexEq` now seeks the index-read cursor's own persisted
traversal position (`state.cursor`) instead of a throwaway one, so a
following `IdxNext` resumes right after the matched entry; both fast
paths add an `IdxNext` + leading-column-still-equal recheck loop that
walks and emits/counts every duplicate-key sibling, falling out the
first time the leading column no longer matches (a `UNIQUE` index's
single match still falls out on its very first `IdxNext`, so this
subsumes #444's original single-probe behavior without a separate
branch). Refs: #450, 009/Req-16.

feat: `ORDER BY`/`LIMIT` on a compound (`UNION`/`UNION ALL`) `SELECT`
(#484) — `compile_select_compound` previously rejected any top-level
`ORDER BY`/`LIMIT` trailing a compound statement. Every arm's projected
rows now feed a shared sorter (reusing the sorter opcodes
`compile_sorted_scan` already uses for a single-table `ORDER BY`)
before `LIMIT`/`OFFSET` and final `ResultRow` emission; a `LIMIT` with
no `ORDER BY` skips the sorter entirely, reusing the simpler
counter-based guards `compile_direct_scan` uses. An `ORDER BY` term
must be an output column name/alias or an ordinal position — matching
real SQLite, which rejects any other expression here even when it only
references an output column name. Refs: #484.

fix: CTE referenced from more than one arm of a compound `SELECT` (#424)
— `compile_select_compound`'s per-arm codegen unconditionally
`OpenRead`'d a resolved-table root page, ignoring
`TableRefKind::Subquery` entirely, so any arm referencing a CTE (or
other `FROM`-subquery) hit `unsupported: table X has an invalid root
page (0)` instead of being materialized. Each arm now branches the
same way the single-`SELECT` path already does, calling
`materialize_from_subquery` per arm on its own cursor. Refs: #424.

fix: recreate a vanished `-wal`/`-shm` during flush instead of failing
(#422) — `Pager::flush_wal_locked` unconditionally called
`wal::WalWriter::open_existing`, which errored if a concurrent
connection (e.g. a real `sqlite3` client auto-checkpointing on close)
had deleted `-wal`/`-shm` out from under this `Pager`, even though its
own `journal_mode` still correctly said `Wal`. Now recreates a fresh
`-wal`/`-shm` pair on that specific `NotFound` case, mirroring
`switch_journal_to_wal`'s from-scratch creation — matches stock
`sqlite3`'s own observed behavior in the same scenario.

feat(cli): REPL mode with `.tables` and `.quit`/`.exit` prefix matching
(#478) — bare `sqlite-rs <file>` (no subcommand) now enters the REPL
directly, matching `sqlite3`'s shell; adds a `.tables` dot-command
(reusing the `tables` subcommand's listing/columnizing logic) and
`sqlite3`-style prefix matching for `.tables`/`.quit` (`.t`, `.ta`, ...
and `.q`, `.qu`, ...); `.exit` remains an exact-only alias for `.quit`.

feat: `ANALYZE` command and cost model for the query planner (#461,
spec 011) — `ANALYZE`/`ANALYZE table-name` populates `sqlite_stat1`
(row counts + per-index `avg_eq`); `Stats`/`PlanCost` (`src/planner.rs`)
estimate scan/index-probe cost from those stats; `choose_join_access`
vetoes a structurally-picked `UNIQUE`-index seek back to a full scan
when the cost model says it isn't actually cheaper, wired live into
the CLI's `query`/`repl` path. A database with no `ANALYZE` history
compiles byte-for-byte as before this change. Filed #470 (join
ordering heuristics) as the real follow-up enabled by this ticket.

docs: close remaining missing_docs gap for docs.rs (#430) — adds `///`
doc comments to the ~890 previously-undocumented public items across
the nested submodules (`parser`, `vdbe`, `btree`, `pager`, `codegen`,
`vfs`, `record`, `schema`) that #428 left out of scope, and enables
`#![warn(missing_docs)]` in `src/lib.rs` so future regressions are
caught by `cargo build`/`clippy`. No logic or behavior changes.

perf: hand-rolled multiplicative hasher for the pager's page-cache
`HashMap` (#457) — page numbers are plain sequential `u32`s, not
adversarial input, so the default hasher's SipHash cost on
`PageCache::entries`'s hot get/insert path was unneeded. No new
dependency (ADR-0022 already ruled that out for this cache);
`point_lookup` bench ~5% faster on both fixtures.

## [0.17.7] - 2026-08-24

fix: cache reassembled payload per row position (#469, #475) —
`TableCursorState::current_payload()` was being called once per
`Column` opcode instead of once per row (a regression introduced by
the lazy-payload change), so an N-column `SELECT` paid for N payload
reassembly passes per row instead of 1. Fixed with the same
once-per-row caching `header_cache` already uses, restoring `full_scan`
to its expected ~1.06x ratio vs the oracle. Also adds regression tests
for the `Payload::Owned` overflow-chain case and page-cache-hit `Rc`
sharing, plus an overflow-payload reassembly benchmark.

## [0.17.6] - 2026-08-24

perf: borrow table row payload from page buffer instead of copying
(#467) — `PageSource::read_page` now returns `Rc<[u8]>` instead of
`Vec<u8>`, so a `PageCache` hit is a refcount bump rather than a copy;
`TableRow::payload` becomes a `Payload` enum that borrows a range of
the shared page for the non-overflow case (zero-copy) and only owns
bytes for the overflow-chain case.

## [0.17.5] - 2026-08-23

perf: reuse row buffer and move registers in `ResultRow` (#465) —
eliminates the per-row `Vec` allocation and per-column `Value` clone by
reusing a `Vm`-owned scratch buffer (mirroring `record_scratch`, #454)
and taking each register's value via `take_register` instead of
cloning it, safe because every scan loop reloads its projected
registers before the next `ResultRow` reads them again. Also corrects
a hand-built DISTINCT VDBE test whose instruction order
(`ResultRow` before `IdxInsert`) didn't match what real codegen
produces.

## [0.17.4] - 2026-08-23

perf: cache parsed row header for repeated `OP_Column` reads (#458) —
table and index-read cursors now parse a row's header (serial types +
byte offsets) once, via `record::parse_header_into`, and cache it
(`RowHeaderCache`) on the cursor state, instead of `decode_column`
re-walking the header from byte 0 on every `Column` opcode against the
same row. The cache reuses its backing `Vec` allocation across rows
(a first, simpler `Option<RowHeaderCache>`-per-row design measured as a
regression on `full_scan` due to alloc/free churn) and is invalidated
solely through `TableCursorState`/`IndexReadState::set_current`, so a
stale cache can never survive a row change. `full_scan` bench ratio
(ours/oracle) improved from 1.39×/2.10× (1MB/50MB fixtures) to
1.25×/1.94×.

## [0.17.3] - 2026-08-23

feat: no-stats query optimizations (#444) — two "always wins, no
ANALYZE/cost model needed" optimizations. Covering-index scan: a
single-table `SELECT` whose `WHERE` is a top-level equality on a
`UNIQUE` index's leading column, with every result column already
carried by that index, compiles to `SeekIndexEq` + `Column` reads
straight off the index cursor, never opening the table cursor.
Index-only `COUNT(*)`: counts via the index cursor (`IdxRewind`/
`IdxNext` or a single `SeekIndexEq` probe) without ever decoding a
table row. LIMIT early-out (#128's third example) needed no new
codegen — the existing `emit_limit_guard`/sorter top-K bound already
cover it. `find_covering_index` is shared between codegen and
`EXPLAIN QUERY PLAN` so the two can't drift apart. Non-unique-index
duplicate-key matches deferred to #450. See spec 009 Requirement 16.

## [0.17.2] - 2026-08-23

fix: correlated scalar subquery equality seeks instead of scanning (#434)
— a correlated scalar subquery's own `WHERE` clause always compiled to
an unconditional `Rewind`/`Next` scan, even when it was a trivially
seekable equality against the subquery table's rowid or a `UNIQUE`
index. Comparing against the pinned sqlite3 oracle's own `EXPLAIN`
output for the reported query showed it uses no caching for this shape
at all — it compiles the equality to a single `SeekRowid` per row.
`compile_scalar_subquery` (`src/codegen/subquery/scalar.rs`) now reuses
`join_access::choose_join_access` (#243's join-level access-strategy
classifier) to take the same fast path; no new VDBE opcode needed.
`#314`'s memoization cache (ADR-0021) stays in place for correlated
subqueries whose `WHERE` isn't a seekable equality. `correlated_subquery`
benchmark: 785x oracle-relative and unmeasurable against `bench_50mb.db`
(blew the 50M-step VDBE guard rail) down to ~14-15x on both fixtures.
See ADR-0027.

## [0.17.1] - 2026-08-23

fix: decode UTF-8/UTF-16 text straight into `Rc<str>` (#441) — text
columns were decoded through an intermediate `String` before converting
to the `Value::Text(Rc<str>)` representation, allocating and copying
twice per text value. `decode_text` (`src/record/decode.rs`) now builds
the `Rc<str>` directly from the decoded bytes, halving text-column
decode allocations. Re-scoped from the ticket's original `Value<'a>`
borrow-from-page-buffer proposal, found premature: the page cache that
design needs is deferred (ADR-0022), and even once built is
LRU-evicting/mutable in place, which is incompatible with a live
borrow under `#![forbid(unsafe_code)]`.

perf: lazy per-column record decoding for the VDBE `Column` opcode (#439)
— `decode_column(payload, idx, encoding)` (`src/record/decode.rs`) walks
the record header to find one column's offset and decodes only that
column's body, instead of `decode_record`-ing the whole row on every
column access. Because WHERE, SET, and SELECT-list column reads all
compile to the same `Column` opcode, and a row a `WHERE` filter rejects
skips its later opcodes via jump-if-false, this gives lazy column
decoding to SELECT, UPDATE, and DELETE uniformly with no codegen
changes. `filter_scan` benchmark (50MB fixture): down from a reported
2.8x gap vs. the oracle to running faster than it (0.65x).

fix: cache the WAL `-shm` fd across a connection's lifetime (#437) —
`Vfs::open_wal_shm` returns a persistent handle `Pager` caches and reuses
for every commit's write-lock claim/`mxFrame` publish, instead of
reopening `-shm` fresh each time. Investigated via a profiling spike
(#438, `tests/spike/011_wal_performance`); ~17.5% faster on a
many-commits-per-connection workload (`concurrent_read_write` benchmark),
though the original `insert_batch_wal_wal` benchmark stays flat since it
opens one connection per commit and has nothing to cache across.

docs: crate-level rustdoc polish (#428) — crate-level `//!` docs, Cargo.toml
publication metadata (`description`, `repository`, `documentation`,
`keywords`, `categories`), and doc comments on previously-undocumented
top-level public items (`DumpError`, `HEADER_LEN`, `HeaderError`,
`VfsError`, `vfs::Result`). Closing the remaining ~884 `missing_docs`
warnings in nested submodules is tracked in #430.

## [0.17.0] - 2026-08-23 — V6.3 Concurrency

Phase V6.3 of epic #354 (V6 Slim), finalizing V6 and unlocking 1.0: real
WAL-mode writes with multi-reader/single-writer concurrency, and the
sqlite3-interop demo that was the epic's stated goal. Closes #388, #389,
#390, #391.

feat: minimal `PRAGMA journal_mode=WAL|DELETE` switching (#388) — a
narrow V6 grammar carve-out (`.openspec/grammar/sqlite.ebnf`, general
PRAGMA support stays deferred to V7) parses only this one pragma
name/value pair, with everything else falling through to a clean
`Unsupported`. Codegen/VDBE wiring (`Opcode::SetJournalMode`) actually
runs the switch: `Pager::set_journal_mode` creates a fresh `-wal`/`-shm`
and flips the header's version bytes going into WAL, or checkpoints
every pending WAL frame, deletes `-wal`/`-shm`, and flips the header
bytes back going to DELETE. Refuses mid-transaction, matching stock
SQLite.

feat: WAL-mode writes actually go through the WAL (#389) — `Pager::flush`
now branches on the tracked journal mode: in `journal_mode=WAL`, every
dirty page is appended as a WAL frame (`WalWriter::open_existing`,
resuming across sessions rather than truncating), the last one marked as
the commit frame, `mxFrame` published to `-shm`, and the writer's own
subsequent reads served by folding the new pages into its in-memory
`wal_pages` overlay — all without ever escalating the main file's SHARED
lock to EXCLUSIVE, so readers are never blocked. A new `WAL_WRITE_LOCK`
(`src/vfs/shm.rs`, `Vfs::claim_wal_write_lock`) serializes concurrent
writers, surfacing contention as the existing `VfsError::Locked` path.
`rollback` in WAL mode was already correct (frames are only ever appended
at commit time). See ADR-0026 for the writer-reopens-and-rescans
trade-off. Un-ignoring `tests/tiers/tier3.rs`'s
`t3_wal_writing_and_live_interop` stays for #390 (live interop with a
real stock `sqlite3` process).

test: sqlite-rs + stock sqlite3 concurrent WAL interop (#390) — the "V6
demo" gate for epic #354: `tests/corpus/wal_concurrent_interop_test.rs`
drives sqlite-rs through the same SQL-level entry point
(`execute_transaction_step`/`compile_statement`, the machinery
`sqlite-rs exec` already wraps) a real caller would, proving all four
scenarios against a live, pinned `sqlite3` process — sqlite-rs writes/
oracle reads, oracle writes/sqlite-rs reads, both alternate commits
(round-tripping WAL frames through each other's checksum chains), and a
checkpoint by either side is read correctly by the other. Un-ignores
`tests/tiers/tier3.rs`'s `t3_wal_writing_and_live_interop`. Also fixes a
real gap this surfaced: `dump::open` (the CLI's `dump`/`query`/`exec`
bootstrap) parsed the database header from the main file's raw bytes
only, which fails for a WAL-mode database whose very first
schema-creating transaction hasn't been checkpointed yet (that page 1's
real content lives only in the `-wal` file) — it now falls back to a
lenient `page_size`-only bootstrap and re-derives the header from the
`Pager`'s WAL-aware read of page 1.

bench: V6 WAL benchmarks (#391) — `tests/performance/v6.rs` (`make
bench-v6` / `cargo bench --bench v6`), four scenarios adapted from the
ticket to what the codebase can measure honestly: `insert_batch_wal`
(1000-row batch INSERT, journal vs WAL mode, driven through the real
`PRAGMA`/`compile_statement`/`execute_transaction_step` SQL path, plus
the pinned oracle's own journal-vs-WAL numbers as a sanity check);
`concurrent_read_write` (a documented sequential interleaving — a
long-open reader's pinned WAL snapshot alongside 20 writer commits, not
a wall-clock-parallel harness; #390's own tests already prove the
non-blocking property, this just measures throughput); `checkpoint_10mb`
(`checkpoint_passive` against a directly-built ~10MB single-commit WAL);
and `cte_reuse_10x` (a CTE referenced 10x via self-join vs. the same
subquery repeated 10x inline). First run (`--sample-size 10`, informal):
`insert_batch_wal` ours journal ~65ms vs WAL ~59ms (a small WAL win, not
the ticket's hoped-for 2.5x — ADR-0026's per-flush `-wal` rescan caps it;
oracle's own journal/WAL numbers are ~3.3ms/~3.4ms, indistinguishable at
this batch size); `concurrent_read_write` ~338ms/20 cycles; `checkpoint_10mb`
~27-31ms; `cte_reuse_10x` cte ~40.9ms vs inline ~40.6ms — parity, not a
win, because `expand_with_clause` rewrites every CTE reference into its
own independent materialization (confirmed by reading
`src/codegen/subquery/cte.rs`), identical cost to inline repetition —
there is no shared-materialization optimization yet (filed as #425).
Also surfaced, not fixed here (out of scope for a benchmark ticket): a
10-way `UNION ALL` of `SELECT count(*) FROM cte` fails compilation past
the first arm ("table cte has an invalid root page (0)") — a
compound-arm/CTE codegen gap, filed as #424. spend: roughly matched the
issue's 1-day estimate.

Also filed from this phase's work: #422 (`Pager` should recover, not
error, when another connection's auto-checkpoint deletes `-wal`/`-shm`
out from under it — found while building #390's interop tests).

spend: V6.3 as a whole ran noticeably over its ~5-day estimate — #388
also had to add a minimal PRAGMA parser (none existed), and #389 had to
make `Pager::flush` genuinely WAL-aware (the write path was entirely
rollback-journal-only beforehand) — both prerequisites the original
per-ticket estimates didn't account for.

## [0.16.1] - 2026-08-23

fix: `parse_insert_stmt` panicked via `expect()` if the first `VALUES` row
were ever empty, even though that path was already unreachable
(`expr_list` always seeds one element before any `Ok`, with `?`
short-circuiting earlier failures). Replaced with the same safe fallback
idiom already used for subsequent rows. The other two `expect()` sites
#409 flagged (`pager.rs`, `btree/master.rs`) turned out to be test-only
code, already correctly lint-allowed — no change needed there. Closes
#409.

## [0.16.0] - 2026-08-23 — V6.2 WAL Core

Phase V6.2 of epic #354 (V6 Slim): the write half of the WAL format, the
wal-index checkpoint-coordination pieces, PASSIVE checkpoint, and crash
recovery / oracle-parity acceptance tests. Closes #383, #385, #386, #387.

feat: WAL frame writer (`WalWriter`) and `WalHeader::new`/`serialize`,
completing the write side of the WAL file format alongside the existing
`WalHeader::parse`/`committed_pages` read path.

feat: wal-index (`-shm`) checkpoint coordination — `WAL_CKPT_LOCK`, a
probe for which reader-mark slots are actually held (bounding checkpoint
progress), and `nBackfill` read/publish.

feat: `checkpoint_passive` — copies committed WAL frames into the main
database file up to the oldest active reader's mark, without blocking on
readers (FULL/RESTART deferred to V7).

test: WAL crash recovery (torn-frame tolerance, checkpoint-mid-write
consistency) and write-path oracle parity — a `-wal` file written by
sqlite-rs recovers correctly through a real, pinned `sqlite3`.

## [0.15.0] - 2026-08-23 — V6.1 SQL completeness

Phase V6.1 of epic #354 (V6 Slim): non-recursive CTEs, `UNION`/`UNION ALL`
compound `SELECT`, and `CREATE VIEW`/`DROP VIEW`.

feat: `WITH` clause (non-recursive CTE) parsing and codegen materialization
— a CTE reference in `FROM`/`JOIN` is rewritten into the same
`TableRefKind::Subquery` shape #257's subquery-in-FROM machinery already
materializes and scans, including multi-CTE chaining, explicit `(col,
...)` lists, and self-joins. `WITH RECURSIVE` parses far enough to report
a clean `Unsupported` rather than a syntax error. Closes #375, #376.

feat: plain `UNION` compound `SELECT` (`UNION ALL` pre-existed via #240),
deduplicated via a shared ephemeral-index cursor reusing `SELECT
DISTINCT`'s guard shape. Closes #377, #378.

feat: `CREATE VIEW`/`DROP VIEW` parsing, view storage in `sqlite_master`,
and query expansion — a view reference in `FROM`/`JOIN` is rewritten the
same way a CTE is, runs after CTE expansion so CTE-of-view and
view-of-CTE both resolve, and detects direct/mutual view-definition
cycles with the same "view X is circularly defined" message stock
SQLite reports. `DROP VIEW` parses but is not yet wired into codegen —
cleanly rejected rather than panicking. Closes #379, #380.

fix: `INSERT ... SELECT` was silently dropping compound-SELECT arms in
codegen, and CTE/view materialization was silently scanning only the
first arm of a compound-SELECT body — both real correctness gaps found
while building this phase, now cleanly rejected instead of producing
wrong results. A CTE substituted into an inline derived table's own
`FROM`, and a view's own body starting with `WITH`, now also resolve
correctly (fixed asymmetries against the already-working paths). Three
extracted-SQL-corpus statements (`[NOT] MATERIALIZED` CTE hint, `WITH`
feeding `INSERT`, single-quoted alias) that the new `WITH`/`UNION`
parsing reached further into were reclassified from `Invalid` to
`Unsupported`, lowering `SELECT_INVALID_BASELINE` from 8 to 3.

test: oracle-diff parity coverage across `tests/corpus/{cte,union,
view}_test.rs` for all of the above, including circular/mutual view
references, CTE shadowing a real table, and clean-rejection pins for
every not-yet-supported combination (compound CTE/view bodies, compound
INSERT source, CTE/view-backed INSERT source, `DROP VIEW`). Closes #382.

Refs: 009/Req-13 (CTE materialization), 009/Req-14 (compound SELECT),
009/Req-15 (view storage and expansion).

## [0.14.1] - 2026-08-23 — V5 review fixes

Follow-up fixes from the combined code-review/security-review pass over
the eight merged V5 PRs (#353 comment thread). Patch release — no new
features, no scope beyond closing gaps the review found in the V5 lock
and transaction-control paths.

fix: `BEGIN IMMEDIATE`/`BEGIN EXCLUSIVE` parsed `TransactionMode` but
`compile_begin` discarded it, so a concurrent writer was only blocked at
`COMMIT` time (via `Pager::flush`'s EXCLUSIVE escalation), not at `BEGIN`
as stock SQLite does. `Transaction`'s `P1` now carries the mode;
`control::transaction` calls the new `Pager::begin_immediate`/
`begin_exclusive` to escalate to RESERVED/EXCLUSIVE right away.
`Pager::flush`/`rollback` release that lock back to `Shared` when the
transaction ends (commit or rollback), including the no-write case (a
`BEGIN IMMEDIATE` immediately followed by `COMMIT`/`ROLLBACK`). New
subprocess interop test (`tests/corpus/begin_immediate_lock_interop_test.rs`)
proves a compiled `BEGIN IMMEDIATE`/`EXCLUSIVE` visibly blocks a live
stock `sqlite3` writer/reader, mirroring `lock_state_interop_test.rs`'s
proof for the raw lock primitive. Closes #395.

fix(pager): `Pager::flush` never used the 5-state lock ladder built for
hot-journal recovery — it only ever held the plain SHARED lock every
`open()` takes, so two writers (or `sqlite-rs` racing a live stock
`sqlite3` process) could both pass the SHARED check and interleave
journal writes/deletes and page writes with no OS-level mutual exclusion.
`flush()` now escalates to EXCLUSIVE (stepping through RESERVED/PENDING)
before touching the journal or main file, and de-escalates back to
SHARED afterward, mirroring `sqlite3PagerCommitPhaseOne`/`Two`. Closes
#398 (Refs #353).

fix: nested `BEGIN; BEGIN;` and a bare `COMMIT`/`ROLLBACK` with no open
transaction silently succeeded instead of erroring like stock SQLite —
a divergence the V5 review flagged as untested. Both now return the
matching stock-`sqlite3` error. Closes #396.

fix(ci): `cargo-mvl-limit` install lacked `--force`, so `Swatinem/rust-cache`
restoring a stale cached binary made the mvl-limit gate flaky in CI.
Closes #394 (chore, CI-only).

Spend: matched the review-fix estimate (see #353 review comment for the
combined analysis this closed out).

## [0.14.0] - 2026-08-22 — V5 Slim: Core Transactions

Epic #353. "ACID with a rollback journal": `BEGIN`/`COMMIT`/`ROLLBACK`
(including `DEFERRED`/`IMMEDIATE`/`EXCLUSIVE`), the 5-state file lock
ladder, rollback-journal write path, hot-journal crash recovery, and the
VDBE transaction opcodes that make it all executable. Spend: matched the
epic's 2-3 week estimate.

feat: parser support for `BEGIN`/`COMMIT`/`ROLLBACK` (`DEFERRED`/
`IMMEDIATE`/`EXCLUSIVE` transaction modes) as first-class statements.
Closes #356.

feat: `src/vdbe/control.rs` gains the transaction opcodes
(`Transaction`/`AutoCommit`) that make compiled `BEGIN`/`COMMIT`/
`ROLLBACK` actually run against the pager instead of just parsing.
Closes #360.

feat: `exec` CLI subcommand runs multi-statement scripts through a single
session, so a script's `BEGIN ... COMMIT` spans multiple statements
against one connection instead of one-shot-per-statement. Closes #358.

feat: minimal `repl` subcommand for interactive multi-statement
`BEGIN`/`COMMIT`/`ROLLBACK` sessions. Closes #365.

test: crash torture test — a kill -9 loop mid-write against the rollback
journal, verifying the database always recovers to a consistent state on
restart. Discharges the epic's "power-cut torture test" acceptance gate.
Closes #361.

bench: `tests/performance/engine.rs` gains four transaction-batching
benchmarks (`insert_single_tx`, `insert_batch_tx_100`,
`insert_batch_tx_1000`, `update_batch_tx`), each running a
`BEGIN`/statement(s)/`COMMIT` session through `execute_transaction_step`
(#360) against a fresh scratch copy of `bench_1mb.db` per iteration, with
an `oracle` counterpart (`rusqlite::execute_batch`) alongside each —
surfacing the per-statement journal/fsync overhead V5's rollback-journal
path pays outside a transaction vs. amortizing it across a batch, and
letting `make bench` check the issue's "within 5× of oracle" batch
criterion directly. Current numbers (`bench_1mb.db`, `--quick`):
`insert_single_tx` 16ms vs oracle 3ms (~5×), `insert_batch_tx_1000` 61ms
vs oracle 4ms (~17×) — batching cuts our per-row cost far faster than
linear, but the ratio to oracle isn't at 5× yet outside the single-row
case; left as a follow-up rather than in scope here. Closes #373. Spend:
matched the small estimate.

fix: `Pager::open` recovered a hot rollback journal from its header magic
alone, with no check for a live second connection — a race against the
oracle's own `hasHotJournal`/`sqlite3PagerSharedLock` (`os_unix.c`/
`pager.c`) behavior. Now non-blocking-probes RESERVED before recovering
(`FileLockState::check_reserved`, `fcntl(F_GETLK)`) and fails with
`VfsError::Locked` if held; on a clear probe, escalates SHARED straight
to EXCLUSIVE, deliberately skipping RESERVED, matching stock SQLite.
Along the way, fixed a related bug the wiring surfaced: hot-journal
recovery opened a second, independent fd to the main db path — exactly
the "`close()` drops all `fcntl` locks on the inode" trap #45 had
flagged and deferred — now consolidated to the one fd `Pager::open`
already holds the lock on. `FileLockState` (#357's 5-state ladder,
previously wired into nothing outside its own unit tests) now backs
`UnixVfsFile`'s lock directly. See ADR-0024. Closes #359 (rescoped from
a duplicate of #172/ADR-0016). Spend: ~1 session, matched the rescoped
1-day estimate.

feat: `src/vfs/lock.rs` gains `LockLevel`/`FileLockState`, a full 5-state
journal-mode lock ladder (UNLOCKED → SHARED → RESERVED → PENDING →
EXCLUSIVE) built on byte-identical `fcntl` byte-range locks, matching
`os_unix.c`'s `unixLock`/`unixUnlock` transition order and PENDING_BYTE
probe semantics. Exposed from `sqlite_rs::vfs` for the follow-up `Pager`
write-path wiring (#45); `Pager`/`VfsFile::lock_shared` are unchanged.
Verified against a live stock `sqlite3` process in both directions
(`tests/corpus/lock_state_interop_test.rs`). Closes #357. Spend:
matched estimate.

test: `src/vdbe/cursor.rs` was the largest coverage gap in the repo
(82.37% lines / 68.39% functions). Adds hand-assembled `Program` tests
for opcodes no current codegen path emits (`Last`, `NullRow`, `IdxLE`)
and for the `CursorTypeMismatch`/`MalformedInstruction` error arms that
accounted for most of the file's missed lines — 84.33% line coverage
after, repo TOTAL 90.91%. No production code changes. Part of epic #234
(V4). Closes #351.

## [0.13.3] - 2026-08-22

fix: 16 codegen call sites (INSERT/UPDATE/DELETE/SELECT/subquery)
defaulted an out-of-range `sqlite_master.rootpage` to `0` instead of
rejecting it — `index_maintenance.rs::open_index_cursors` already had
the correct `CodegenError::Unsupported` rejection for an index's root
page, but 16 other table/index root-page sites used the naive
`i32::try_from(...).unwrap_or(0)` shortcut instead. A corrupt or
adversarial `sqlite_master` entry could silently produce a cursor
pointed at page 0 (the reserved header page) instead of a compile
error — wrong results, no diagnostic. Factored the existing check into
two shared helpers (`valid_table_root_page`, `valid_index_root_page`)
and applied them everywhere. Found via `make silent-swallow`'s #342
audit (#349).

fix: honor DISTINCT in aggregates and coerce TEXT/BLOB in sum()/avg()
— `count(DISTINCT x)`/`sum(DISTINCT x)`/`avg(DISTINCT x)` previously
silently ignored `DISTINCT`, and `sum()`/`avg()` skipped TEXT/BLOB
inputs instead of coercing them to their numeric-prefix value per
SQLite's own text-coercion rule (R-29052-00975). Found via the
vendored sqllogictest suite, which now passes in full (#348).

perf: hoist uncorrelated WHERE-clause subquery in aggregate scan too
(#322, #323) — extends #314's per-outer-value memoization/hoisting to
the aggregate-scan codegen path, not just plain SELECTs.

perf: UPDATE/DELETE rowid/index-equality seek fast path (#336) — point
mutations no longer pay a full table/index scan to find the target row.

perf: in-place leaf/index cell splice instead of collect-all/rewrite-all
on single-row mutation (#337) — single-row INSERT/UPDATE/DELETE on
table and secondary-index leaf pages now splices the cell-pointer array
in place (O(1) relative to the page's other cells) instead of decoding
and rewriting every cell on the page, when there's enough contiguous
free space. Adds real freeblock-chain and fragmented-byte bookkeeping
per `fileformat2.html` (previously always written zero — see
`.openspec/adr/0023-leaf-cell-splice.md`). `delete` always takes the
O(1) path; `insert`/`update` fall back to the existing full-rebuild
path when the page's contiguous gap is too small, which also
defragments the page as a side effect.

(#338, hash-based aggregation for unindexed GROUP BY, investigated and
closed as not applicable — stock sqlite3 has no hash-aggregation
strategy either; it always sorts via a temporary B-tree for the
unindexed case, so our existing sort-then-group codegen already
matches oracle's algorithm choice. Left open as an `enhancement` —
optional follow-up if profiling ever justifies closing the constant-factor
gap independently of the algorithm.)

## [0.13.2] - 2026-08-22

fix: aggregate functions (`count`/`sum`/`avg`/`min`/`max`) combined
with a JOIN — `SELECT a.name, count(*) FROM a JOIN b ON ... GROUP BY
a.name` previously failed with `unsupported: aggregate function
count`, despite this exact combination being the V4 epic's (#234)
stated acceptance gate. Generalizes `compile_grouped_scan`'s
sort-then-group codegen shape to a joined `Scope`, the same way #250
generalized `ORDER BY`/`DISTINCT`. Bounded MVP: `GROUP BY`
terms/aggregate arguments must be bare columns, result columns must be
`*`/`table.*`/a bare column/a whole aggregate call, and `HAVING`
combined with a JOIN stays unsupported. Fixes
`tests/tiers/tier3.rs::t3_multi_table_joins_and_aggregates` (previously
routed around the gap instead of testing it) and activates
`tests/parity/v04.rs` (#333, #335).

## [0.13.1] - 2026-08-21

chore: `make lint` now covers `[[test]] test = false` targets
(`corpus`/`parity`/`sqllogictest`/`point_lookup_perf`) — the same
convention that opts them out of the default `cargo test` run also
opted them out of `cargo clippy --tests`/`cargo fmt`, letting compile
errors and lint violations accumulate invisibly (a stale `FromClause`
field reference in `sqllogictest/runner.rs` was one such compile
error, fixed separately). Fixed the 23 accumulated violations this
uncovered: `&PathBuf` parameters narrowed to `&Path` (9 sites across
`tests/corpus/`, 1 in `tests/performance/point_lookup.rs`), two
`indexing_slicing` sites in `point_lookup.rs` replaced with
`.get()`/destructuring, a `type_complexity` violation in
`tests/parity/{driver,v02}.rs` factored into a `QueryRunner` type
alias, and three violations in `tests/sqllogictest/{runner,format}.rs`
(`manual_split_once`, `unnecessary_sort_by`, `enum_variant_names`).
`make lint` now runs `cargo clippy` against these four targets
explicitly (named, not a wildcard — matching how `--tests` itself
isn't one either).

fix: `tests/sqllogictest/runner.rs` failed to compile (`&FromClause`
has no `name` field — a stale reference left over from #276's
`TableRef`/`FromClause` refactor, invisible to `cargo build
--all-targets`/`make lint` since `sqllogictest` is a `test = false`
target neither reaches). Fixed by reusing
`codegen::resolve_from_table_schema` (already used elsewhere for the
same lookup) against `from.first`, skipping multi-table `FROM`
(out-of-slice for V2, per this module's own doc comment) instead of
silently mis-resolving it. Also caught two other pre-existing gaps in
the same blind spot: an unhandled `CodegenError` variant (again
invisible to normal `cargo build`) and three clippy violations in
`tests/sqllogictest/{runner,format}.rs` (`make lint` doesn't reach
`test = false` targets at all — filed as #299 to close that gap
properly). Once compiling, the slice's actual coverage jumped from
349→1000 passing queries (15%→43% of the vendored corpus) — the
committed `tools/sqllogictest-status.json` baseline had gone stale
while this runner was broken, silently, since CI's own `sqllogictest`
step is `continue-on-error: true` (informational, not a gate).

fix: `LIMIT 0` returned every matching row instead of none. Every
scan shape's LIMIT counter (`src/codegen/select/limit_scan.rs`'s
`emit_limit_guard`, reused by joins, aggregates, and the `SeekRowid`
fast path) checked `DecrJumpZero` *after* emitting a row, so a `LIMIT
0` counter — starting at exactly `0` — could never stop anything
before the first row already leaked through. Restructured as a
check-before-act guard (mirroring `emit_offset_guard`'s existing
shape): `IfNotZero` gates whether a row is emitted at all, decrementing
only while positive, so a negative `LIMIT` (SQLite's "no limit"
convention) still falls through unbounded exactly as before. Caught
while benchmarking #129, unrelated to that ticket's sorter change.

chore: cap ephemeral table/index materialization at 1M rows (#269).
`EphemeralTableState.rows`/`EphemeralState.entries` (`src/vdbe/cursor.rs`)
backed a plain in-memory `Vec`/`BTreeMap` with no ceiling — a
subquery-in-FROM (#257) or a correlated `IN (SELECT ...)` rebuilding its
ephemeral index per outer row (`compile_in_subquery`) could grow memory
without limit. Adds `ExecError::EphemeralRowLimitExceeded` and a
`MAX_EPHEMERAL_ROWS` constant checked at both insert sites, following the
existing hardcoded-limit pattern (`MAX_REGISTERS`, `MAX_STEPS`) rather
than a new configurable-limits mechanism.

test: assert an aggregate in tier3's joins-and-aggregates stub (#267).
`t3_multi_table_joins_and_aggregates` claimed aggregate coverage by name
and ignore-reason but exercised only JOIN + ORDER BY/DISTINCT/
`INSERT ... SELECT`. Added a `GROUP BY` + `count(*)` assertion; aggregate
functions combined with a JOIN aren't supported by codegen yet, so it
runs against a single table rather than the joined query — tracked as a
known coverage gap in #268.

perf: bounded top-K sorter for `ORDER BY ... LIMIT N` (#129). The
ephemeral sorter (`src/vdbe/sorter.rs`) previously buffered and sorted
every matching row before `LIMIT` ever applied — a full `O(N log N)`
sort regardless of how small `LIMIT` was. `SorterOpen` now accepts an
optional bound register (`P2`/`P5`, wired from
`src/codegen/select/limit_scan.rs::compile_sorted_scan` via the
existing-but-previously-unused `OffsetLimit` opcode's `LIMIT +
max(OFFSET, 0)` / `-1`-means-unbounded convention), and `SorterInsert`
maintains a binary max-heap capped at that bound instead of an
ever-growing buffer — O(log bound) per insert, provably lossless (a
row that loses the eviction comparison can never land within the
final `LIMIT` output). Skipped whenever `DISTINCT` is present (it
dedupes *after* the sort, so bounding beforehand could evict a row
DISTINCT would have kept). ~40% faster on the tier-1 benchmark's
`order_by_limit` case; a linear (non-heap) worst-row scan was tried
first and *regressed* performance whenever `bound` exceeds `log2(row
count)` — a genuine dead end worth noting for anyone revisiting this.
Index-ordered scanning (skip the sorter entirely when an index matches
the `ORDER BY` column) is a separate, larger follow-up — see #296.

refactor: consolidate aggregate codegen onto `AggStep`/`AggFinal` (#263,
ADR-0019). `src/codegen/select/aggregate.rs`'s `GROUP BY`/plain-aggregate
compilation (`compile_grouped_scan`) now emits `Opcode::AggStep`/
`Opcode::AggFinal` — implemented since #241/#242 but never actually
emitted by codegen (ADR-0018 tracked this gap) — instead of the
`AggKind`/`AggSlot` hand-rolled register-arithmetic scheme (`reset_agg`/
`accumulate_agg`), now retired. Surfaced two VM-side gaps along the
way: `AggStep`'s `min`/`max` comparisons were hardcoded to `BINARY`
collation (the same class of bug #265 just fixed in the old scheme —
fixed here via a new `P4::AggFunc{name, arity, collation}` descriptor),
and there was no way to reset an aggregate-context slot for a new
`GROUP BY` group reusing the same slot number (fixed via `AggStep`'s
previously-unused `P5` operand as a reset flag). See ADR-0019 for the
full design and rejected alternatives (a dedicated reset opcode,
threading comparison affinity through as well, folding in plain
non-`GROUP BY` aggregate support).

fix: EphemeralTable `Insert` decode uses the database's real text
encoding (#266). `src/vdbe/cursor.rs`'s subquery-in-FROM materialization
path hardcoded `TextEncoding::Utf8` instead of `db.header.text_encoding`
like every other decode site in the file; a UTF-16 database queried
with a subquery in FROM would misdecode text or surface a generic
`MalformedInstruction`. Falls back to UTF-8 only when no db is attached
(pure in-memory ephemeral use, e.g. DISTINCT). Regression test added
directly against the opcode handler, since building a real UTF-16
fixture through the SQL engine isn't possible yet — `MakeRecord`
(`src/vdbe/result.rs`) still hardcodes UTF-8 on the encode side, a
separate, wider-scope gap left for a follow-up ticket.

test: aggregate/join/subquery edge-case coverage from the v0.13.0
review (#268). Adds 11 tests across `tests/codegen/select_test.rs`,
`tests/corpus/union_test.rs`, `tests/corpus/join_test.rs`, and
`tests/corpus/subquery_test.rs`: HAVING-filters-all-groups, UNION ALL
arm type/affinity mismatch (no coercion, verified), the LEFT/RIGHT/
FULL JOIN `WHERE ... IS NULL` anti-join idiom, two-level-deep
correlated subqueries, and multi-column `IN`/`NOT IN` subquery edge
cases (zero-row result, NULL tuple component) — all against working
functionality. Two sub-items turned out to be missing features, not
test gaps, and are documented as clean `Unsupported` rejections rather
than fixed here: aggregates with no `GROUP BY` at all (even
`count(*)`), and `FULL JOIN` combined with `ORDER BY`/`DISTINCT`/
`LIMIT`; also newly discovered, a correlated subquery nested inside a
FROM-subquery's own SELECT list. Tracked as follow-on tickets rather
than expanding this test-only ticket's scope.

refactor: tighten `src/btree` and `src/codegen` module layout (#273,
#276). Pure module reorganization, no behavior change: `src/btree/`
groups table b-tree write ops (`insert`/`delete`) under `table.rs` +
`table/`, and index write ops (`index_insert`/`index_delete`) under
`index.rs` + `index/`, giving both write paths symmetric naming;
`ddl.rs` renamed to `schema.rs`. `src/codegen/` groups DDL codegen
(`create_table`/`drop_table`/`create_index`/`drop_index`) under
`ddl.rs` + `ddl/`; `select.rs` (4033 lines) split into a facade plus
9 sub-modules under `select/` (`entry`, `joins`, `join_full`, `eqp`,
`join_access`, `order_by`, `projection`, `limit_scan`, `aggregate`),
each ~1000 lines or fewer. Spec 006 `Implementation:`/`Tests:` path
citations updated for the moved btree files.

feat: `.tables [PATTERN]` shell parity for the `sqlite-rs tables` CLI
subcommand (#177). Lists tables *and* views from `sqlite_master` (a new
`read_table_and_view_names` schema reader, `src/schema/ddl_reader.rs`,
bypasses `read_schema`'s DDL parsing entirely since `.tables` needs
neither), excludes internal `sqlite_%` names, accepts an optional LIKE
`PATTERN` argument (reusing `vdbe::like_match`), and renders in
`sqlite3`'s multi-column, space-padded `.tables` layout — verified
byte-for-byte against the pinned 3.53.4 oracle. `temp.`-prefixed temp
tables remain deferred (needs the V3+ write path's temp-database
support).

refactor: split `src/bin/sqlite-rs.rs` into modules, move dispatch into
the library (#292). `src/codegen/dispatch.rs`'s `compile_statement`/
`leading_keywords` now return a library `DispatchError` instead of an
`ExitCode`, so they're usable without depending on the binary crate;
the CLI itself splits into per-command modules
(`src/bin/sqlite-rs/{main,dump,tables,query,exec,common}.rs`) behind a
thin `main.rs` dispatcher. No behavior change. Spend: matched estimate
(small).

refactor: dedupe join/subquery codegen (#270, #308). Four independent,
behavior-preserving extractions: `compile_join_level_traverse`
(`src/codegen/select/joins.rs`) factors the shared nested-loop/
outer-join traversal out of `compile_join_level` and
`compile_join_level_for_sort` (#250's `ORDER BY`+JOIN sorted path),
parameterized over row emission via a `leaf` closure;
`compile_in_subquery` becomes a thin one-element wrapper around
`compile_in_subquery_multi`; NATURAL/USING join-constraint synthesis is
unified into one `resolve_join_constraint` helper shared by
`compile_select_joined_scan` and `compile_full_join_two_table`.
**Latent-bug fix surfaced along the way:** `compile_join_level_for_sort`
had silently diverged from `compile_join_level` and never gained #243's
seek optimization — a joined query with `ORDER BY` downgraded an
otherwise-seekable `SeekRowid`/`SeekIndexEq` point lookup to a full
`Rewind`/`Next` scan. Both paths now share one traversal, so the
optimization applies unconditionally; verified via a new
`EXPLAIN QUERY PLAN` regression test.

perf: index-ordered scan for `ORDER BY ... LIMIT` (#296, #309,
ADR-0020). `find_ordering_index` (`src/codegen/select/index_scan.rs`)
looks for a single index on the FROM table whose column order is a
prefix match (forward or exactly-reversed) for the requested `ORDER BY`
terms — `BINARY` collation only, and an explicit `NULLS FIRST`/`LAST`
must agree with the direction's default. When found,
`try_compile_index_ordered_scan` walks the index directly (new
`IdxRewind`/`IdxLast`/`IdxNext`/`IdxPrev` opcodes + `IdxRowid` +
`SeekRowid`) with `LIMIT`/`OFFSET` as an early-exit guard — no
buffering, no sorter — ahead of the existing `compile_sorted_scan`
fallback. `IndexCursor` gained `last()`/`prev()`, the mirror of its
existing `first()`/`next()`.

feat: `ORDER BY` and `DISTINCT` combined with `FULL JOIN` (#288, #307).
Extends `compile_full_join_two_table`'s two-pass emitter: `DISTINCT`
threads a `distinct_cursor` through all three emission sites (matched,
left-nulled, right-unmatched), reusing the existing ephemeral-index
dedup guard; `ORDER BY` routes all three through a new
`emit_full_join_sort_row`, buffering into a sorter cursor (mirroring
the ordinary join tree's `compile_joined_sorted_scan` split) with a
fourth pass draining the sorter and applying `LIMIT`/`OFFSET`
post-sort. `DISTINCT` + `ORDER BY` together remains rejected, matching
the ordinary join tree's existing restriction. Spend: ~2x estimate —
needed sorter-buffering plumbing in the two-pass emitter rather than a
config flag.

fix: correlated subquery inside a `FROM`-subquery's own `SELECT` list
(#289, #311). `materialize_from_subquery`'s single-table (non-join)
path compiled the subquery's `SELECT` list against a `Scope` catalog
limited to its own resolved `FROM` schema(s) instead of the full outer
catalog, so any nested subquery referencing another table hit a
catalog-visibility rejection — the already-correct joined-FROM path
was unaffected. Spend: matched estimate (medium).

feat: aggregates with no `GROUP BY` (#287, #313). `SELECT count(*)/sum/
avg/min/max FROM t;` (no `GROUP BY`) now compiles and executes on both
populated and empty tables, as a thin extension of the existing
sort-then-group machinery (`compile_grouped_scan`): every row belongs
to one synthetic implicit group, and a new `implicit_group: bool`
parameter ensures a zero-row table still flushes exactly one result
row (`count(*) = 0`, other aggregates `NULL`) rather than zero rows.
`HAVING` without `GROUP BY` is now accepted too, filtering that single
implicit group. `.openspec/grammar/sqlite.ebnf` corrected: `HAVING` is
parse.y's own independent `having_opt`, not nested under
`groupby_opt`. `total`/`group_concat` remain unsupported (no `AggState`
accumulator yet); the joined-select path is untouched. Spend: matched
estimate (medium).

bench: add V4 join/aggregate/subquery scenarios to the tier-1 engine
bench (#301). Extends `tests/performance/engine.rs` with `join`,
`group_by_agg`, and `subquery` scenarios now that V4 phase 1 landed
(#235), plus a fixed-size `bench_lookup` dimension table and
`bench_data.bucket` column in `gen_fixtures.sh --bench`.
`compile_ours` now dispatches single-table vs `JOIN` the same way
`src/bin/sqlite-rs/query.rs` does. First measured ratios
(`bench-status.json`) surfaced two follow-ups, filed rather than fixed
here per this ticket's scope: an uncorrelated subquery re-executed
(and its ephemeral index rebuilt, for `IN`) on every outer row instead
of once (#306), and `join`/`GROUP BY` ratios (14-26x) exceeding #111's
1.5-3x calibration (#310). Spend: roughly matched the 120k token
estimate.

fix: hoist uncorrelated `WHERE`-clause subquery out of the outer scan
loop (#306, #315). A scalar or single-column `IN (SELECT ...)`
subquery in a single-table `WHERE` clause was re-materialized on every
outer row even when uncorrelated — severe enough to hit the 50M-step
VDBE guard rail on large tables (#301's bench run). Adds a static,
conservative correlation check (`subquery_is_correlated`/
`walk_expr_for_correlation` — anything uncertain is treated as
correlated, which only ever suppresses the optimization) plus a hoist
pass: an uncorrelated top-level `WHERE` conjunct materializes once,
before the scan's `Rewind`, via a new pointer-identity-keyed
`Scope::hoisted` map, instead of inline per row. Deliberately narrow —
only an exact `expr IN (SELECT ...)` or scalar-subquery comparison
conjunct is recognized; `OR`/`NOT`/deeper nesting, multi-column `IN`,
correlated subqueries, and the joined-query `WHERE` path all fall
through unchanged. (A follow-up commit fixed an `unreachable!`-adjacent
`make mvl-limit` gate violation the hoist's correlation-walk helper
introduced.) Spend: roughly matched the ~200k token estimate.

chore: design note + benchmark for correlated-subquery
rematerialization cost (#303, ADR-0021). ADR-0021 documents deferring
a full coroutine rewrite in favor of a scoped follow-up (memoize a
correlated subquery's result keyed on its outer-referenced value(s),
reusing #306's correlation walk). Adds a `correlated_subquery` bench
scenario to `tests/performance/engine.rs` demonstrating the current
per-outer-row re-materialization cost, guarded to `bench_1mb.db` only —
it blows the VDBE step cap against `bench_50mb.db`, itself evidence of
the unbounded cost. No fix in this ticket, per its own acceptance
criteria.

feat: aggregate function inside a scalar/correlated subquery (#304,
#318). `SELECT (SELECT max(x) FROM t) FROM t LIMIT 1` (and the
correlated form) previously failed with "unsupported: aggregate
function max" — a scalar subquery's projected expression compiled
through `compile_value`'s plain, aggregate-rejecting path instead of
#287's aggregate machinery. `compile_scalar_subquery` now detects an
aggregate call in the subquery's projection and routes through
`compile_grouped_scan` as an implicit whole-table group, capturing the
result via a `Copy` into the destination register;
`compile_grouped_scan` gained an `outer_scope: Option<&Scope>`
parameter so a correlated subquery's `WHERE` clause still resolves
against the enclosing scope. **Bug fix found along the way:**
`AggFinal` never cleared its `agg_contexts` slot after finalizing —
invisible for a top-level query (compiled once), but a correlated
aggregate subquery reuses the same slot per outer row, so a zero-row
invocation finalized against the *previous* row's leftover accumulator
instead of a fresh NULL/0 (`Vm::clear_agg_context` added). Spend:
within estimate (medium).

perf: index-ordered scan for `GROUP BY` (#310, #316). `compile_grouped_scan`
always buffered the whole `WHERE`-matching table into a sorter before
aggregating, even when the `GROUP BY` columns already had a covering
index producing rows in the right order (#301's bench found 14-26x
tier-1 ratios on `group_by_agg`). `try_compile_index_ordered_group_by`
mirrors #296's `ORDER BY` MVP: walks a matching index directly
(`IdxRewind`/`IdxNext` or `IdxLast`/`IdxPrev` + `IdxRowid` +
`SeekRowid`), feeding the same boundary-detection/accumulate/flush
logic `compile_grouped_scan`'s pass 2 already has — no sorter, no
`MakeRecord`, no buffering. Guardrails mirror #296's own MVP: no
`WHERE` clause, an ordinary rowid table, every `GROUP BY` term a bare
column. (A follow-up commit precomputed `group_col_indices` up front to
satisfy a `make mvl-limit` gate the per-row loop's `unreachable!()`
branch had violated.) Closes the `group_by_agg` half of #310 only —
the `join` per-row dispatch-overhead half is split into #317. Spend:
roughly matched the ~150k token estimate.

docs: ADR-0022 — profile #317's join ratio, find the missing page
cache. #317's own scope was profiling, not fixing. Instrumenting
(rather than accepting the provisional "per-row VDBE dispatch
overhead" hypothesis from #310/#317) found the real cause:
`Pager`/`VfsPageSource` have no page cache at all — every `read_page`
does a fresh syscall + allocation, unconditionally, and `join`'s
per-row `SeekRowid` re-descends the b-tree from the root on every
outer row, re-reading the same root/interior pages hundreds of
thousands of times on the 830k-row fixture. Ruled out the
dispatch-overhead hypothesis via direct instruction-count comparison:
`full_scan`'s per-row body has *more* instructions than `join`'s yet
the *better* ratio (~3.4x vs ~14-16x). No code fix in this ticket
(matches #317's own acceptance criteria); filed #320 as the scoped,
fully-designed follow-up ("add a bounded page cache to `Pager`'s read
path"). Spend: roughly matched the profiling-scope estimate.

## [0.13.0] - 2026-08-21

feat: zero-arity scalar functions + FROM-less SELECT (#136, #260,
V4 phase 1 epic #235). Registers `sqlite_version()` as SQLite's real
zero-arity scalar function, exercising codegen's previously-untested
`FunctionCall` zero-arg branch through a real compiled query.
`compile_select_no_from` (`src/codegen/select.rs`) adds FROM-less
`SELECT <expr>[, ...]` support — the normal way built-ins like
`sqlite_version()` are called (`SELECT sqlite_version();`) — compiling
the column list once against an empty schema and emitting exactly one
row with no cursor/scan bracketing; `*`/`tbl.*` and any clause
presuming a table (WHERE/GROUP BY/HAVING/ORDER BY/LIMIT/DISTINCT/
compound) is rejected as unsupported. Wired into the `sqlite-rs` CLI's
`query` subcommand too, which previously hard-refused any FROM-less
`SELECT` before ever reaching codegen.

`UPDATE`/`DELETE` subquery catalog threading + multi-column `IN` (#251,
V4 phase 1 epic #235): `compile_update`/`compile_delete` gained
`_with_catalog` variants (mirroring `compile_select_with_catalog`'s
shape) so a subquery in a `SET` value or `WHERE` clause that references
a table other than the statement's own target now resolves instead of
failing at codegen time with an empty catalog. Also lands multi-column
`IN` (`(a, b) IN (SELECT x, y FROM t)`): a new `ExprKind::InSubqueryMulti`,
parsed via a token-scan-gated speculative lookahead (so the
tuple-vs-grouping-paren ambiguity doesn't regress parser performance on
deeply nested plain expressions), and codegen generalizing the existing
single-column `IN`'s ephemeral-index machinery to an N-column key.
`ANY`/`ALL`/`SOME` quantified comparisons, originally also scoped into
#251, were dropped entirely — verified against the pinned oracle that
SQLite has never implemented that syntax (Postgres/MySQL/standard-SQL
only); subqueries in `FROM` split off to a follow-up (#257).

fix: two computed result columns collide (#141). `Copy` (`r[P2] =
r[P1]`) harvested from the pinned oracle (`SELECT count(*), sum(price)
FROM products`, alongside `AggStep`/`AggFinal` riding the same
harvest — see ADR-0018) closes the gap `Opcode::Copy` was already
hand-added for during #208 but never wired into `compile_row_values`'s
contiguity check. `compile_row_values` (`src/codegen/select.rs`) now
computes each result column first, and only reserves a fresh
contiguous run + `Copy`s into it when the columns didn't land
contiguously on their own — no more outright rejection of e.g.
`SELECT i + 1, i - 1 FROM t` or `SELECT coalesce(i, -1), ifnull(s, 'z')
FROM t`. `emit_branch_into` (`src/codegen/expr.rs`) now accepts
arbitrary CASE branch expressions the same way, and `FunctionCall`
argument compilation gained the identical reserve-and-copy fallback
for the same underlying contiguity check under a different name.

JOIN: remaining forms (#250, V4 phase 1 epic #235), closing out what
#237 deferred. Parser: `NATURAL` joins, `RIGHT`/`FULL [OUTER] JOIN`,
`USING (col, ...)`, and comma-style `FROM a, b` (parsed as CROSS-join
sugar, which needed no codegen work — it already compiles through
#237's CROSS JOIN path). Codegen: `NATURAL`/`USING` synthesize the
`ON`-equivalent equality constraint from schema column names and
de-duplicate the shared column in `SELECT *` output; `RIGHT JOIN`
reorders the execution loop nesting so the right-hand table becomes
outer (`A RIGHT JOIN B == B LEFT JOIN A`), generalizing the `LEFT
JOIN` matched/null-extension machinery; `FULL JOIN` adds a second
ephemeral-index-tracked pass (same mechanism as `DISTINCT`) for
right-side-unmatched rows. `ORDER BY`, `DISTINCT`, and
`INSERT ... SELECT` are now all generalized to work with a JOIN in the
`FROM` clause (previously rejected outright). Deliberate, narrower-
than-full-generality scope, each returning a clean `Unsupported` error
rather than a silently wrong result: only one `RIGHT JOIN` per `FROM`
clause; `FULL JOIN` restricted to a single two-table case; a computed
`SELECT`-list expression combined with a joined `ORDER BY`; and
`DISTINCT` + `ORDER BY` combined with a JOIN. `tests/tiers/tier3.rs`'s
`t3_multi_table_joins_and_aggregates` (the tier-contract acceptance
gate for #250) is un-ignored. Spend: ran well past the ticket's
"Medium" estimate once `RIGHT`/`FULL JOIN`'s loop-reordering and
two-pass tracking turned out to need real architectural generalization
rather than a local tweak.

Planner: join-level WHERE/`ON` equality index selection (#243, V4 phase
1 epic #235). An inner join table's `ON` equality against the outer
table's rowid, or against a `UNIQUE` single-column index, now compiles
to a `SeekRowid`/new `SeekIndexEq`+`IdxRowid`+`SeekRowid` point lookup
instead of an unconditional `Rewind`/`Next` full scan (`choose_join_access`,
`src/codegen/select.rs`) — `LEFT JOIN`'s null-extension is unaffected.
Two new VDBE opcodes (`SeekIndexEq`, `IdxRowid`) and a new real
secondary-index read cursor (`CursorSlot::IndexRead`, `OpenRead` with
`P5` nonzero) back the index-seek path; non-unique indexes and compound
(`AND`) `ON` conditions still fall back to a full scan (deliberately
narrow, mirroring #137's `try_compile_rowid_seek`). `EXPLAIN QUERY
PLAN` (pulled forward from its original V7 grammar slot — see
`.openspec/grammar/sqlite.ebnf`'s `explain-stmt`, V4 now) reports
`SCAN`/`SEARCH ... USING ...` per table so the planner's choice is
observable from the CLI (`query "EXPLAIN QUERY PLAN <select>"`); bare
`EXPLAIN` (opcode dump) is unchanged, still served by `-explain`.
Spend: ~2x the ticket's original estimate, because scoping surfaced
three pieces of missing infrastructure (index-seek opcode, EXPLAIN
QUERY PLAN parsing, and the V7→V4 grammar pull-forward) beyond the
issue's original "basic WHERE analysis" framing.

Subqueries in `FROM` (#257, V4 phase 1 epic #235, split off from #251).
Parser: `table-ref` gains a `"(" select-stmt ")" AS identifier`
alternative (`TableRef` is now `Name`/`Subquery`-shaped in the AST).
Codegen: a `FROM`-subquery materializes into a new VDBE table-mode
ephemeral cursor (`OpenEphemeral` with `P5` nonzero — `Rewind`/`Next`/
`Column`/`Insert`/`Rowid` now work against an in-memory row list with
assigned rowids, alongside the existing index-mode ephemeral cursor
DISTINCT already used), bound into `Scope` via a synthetic `TableSchema`
derived from the subquery's own projected columns — then scanned like
any real table. Works standalone, as one slot of a joined outer `FROM`,
and when the subquery's own `FROM` itself has a `JOIN`. `ANY`/`ALL`/
`SOME` remain out of scope (never implemented by the pinned oracle).
Spend: ran past the issue's own "Medium-Large" estimate — the ephemeral
table-mode cursor (Rewind/Next/Insert/Rowid over an in-memory row list)
didn't exist yet and had to be added to the VDBE engine, beyond the
issue's parser/codegen framing.

`UNION ALL` compound `SELECT` (#240, V4 phase 1 epic #235): parser
chains `SELECT ... UNION ALL SELECT ...` arms into `Select::compound`,
with `ORDER BY`/`LIMIT` binding to the whole compound statement rather
than any one arm. Codegen emits each arm's scan/`ResultRow` block back
to back with per-arm cursor numbers (`ScanCursors::for_arm`),
concatenating with no deduplication and no shared sort/merge step. A
column-count mismatch between arms is rejected at compile time. Plain
`UNION` (dedup)/`INTERSECT`/`EXCEPT`, joins/subqueries within an arm,
and `ORDER BY`/`LIMIT` on the compound statement remain out of scope
(deferred to V4 phase 2 or later).

VDBE `AggStep`/`AggFinal` opcodes (#241/#242, V4 phase 1 epic #235): a
`count`/`sum`/`avg`/`min`/`max` accumulator registry dispatched by a
`"name(arity)"` P4 descriptor, mirroring the existing `Function`
opcode's registry-dispatch shape, plus a per-slot aggregate-context
table on `Vm` addressed the same way `cursors` is. `avg` mirrors
`sum`'s integer/real promotion and always finalizes REAL (or NULL on
zero non-null rows); `min`/`max` compare via `vdbe::compare::compare`
under SQLite's type-ordering rules (NULL < INTEGER/REAL < TEXT <
BLOB), skipping NULL args like `count(x)`. Not wired into GROUP BY
codegen — #239 (merged first) took a different, opcode-free approach
for `count`/`sum`/`avg`/`min`/`max` (reusing existing arithmetic/compare
opcodes), so `AggStep`/`AggFinal` currently have no caller in
`src/codegen/`; they stand as tested, spec-backed (spec 009 Requirement
12) VM primitives for future use.

`GROUP BY` / `HAVING` (#239, V4 phase 1 epic #235): parser accepts
`GROUP BY` (single/multi-column, arbitrary expressions) and `HAVING`.
Codegen groups via the existing `Sorter*` opcode machinery
(sort-then-group, mirroring SQLite's own `select.c` shape) and
accumulates `count`/`sum`/`avg`/`min`/`max` per group from existing
arithmetic/compare opcodes rather than new dedicated `AggStep`/
`AggFinal` opcodes. `HAVING` and aggregate result columns compile
against a synthetic per-group record via AST substitution of
aggregate calls into synthetic column references, reusing
`compile_row_values`/`compile_cond` unchanged. `GROUP BY`/`HAVING`
combined with `ORDER BY`/`DISTINCT` in the same `SELECT`, and
aggregates beyond `count`/`sum`/`avg`/`min`/`max`, are out of scope for
this ticket.

Pre-tag `/review` of the full v0.12.3..v0.13.0 diff (epic #235) found no
tag-blocking issues; follow-ups filed as #265–#271 (MIN/MAX collation,
subquery-in-FROM text-encoding, tier3 aggregate-stub gap, edge-case test
coverage, ephemeral-materialization sizing, join/subquery codegen dedupe,
scope-gap tracking confirmation).

## [0.12.3] - 2026-08-20

`ORDER BY` of a rowid-alias column crashed ("Rowid: cursor slot 2 is a
pseudo cursor, not a table cursor") — `emit_column_read` always emitted
`Opcode::Rowid` for the rowid-alias column, valid only against a real
table cursor, but `ORDER BY`'s second pass re-reads each row from a
materialized `OpenPseudo` cursor. Fixed in both call paths that hit it
(`compile_row_values`'s `Column` and `Expr` arms), by reading the
already-resolved rowid value back via `Opcode::Column` instead when the
cursor is the post-sort pseudo cursor. Found via a new full-lifecycle
regression test (`tests/corpus/cli_write_test.rs`) exercising the CLI
end to end: schema -> insert -> update -> delete -> select -> export.

Also fixed a genuine compile break in `tests/sqllogictest/runner.rs`
(non-exhaustive `CodegenError` match missing the `RowShapeMismatch`
variant #195 added) that meant `make sqllogictest` never actually
built — fixing it let `select1.test` run for the first time.

Assurance tooling: `make mutants` (cargo-mutants scoped to
`src/{record,btree,vdbe}/*.rs`, reporting to `target/mutants.out`) and
`make verify` (`coverage-gate` + `deny` + `mvl-limit` + `mod-files`
chained, recording the passing commit to `target/verify.json`) are now
wired into `tools/assurance.py`'s Evidence/Verification sections —
mutation score and a commits-since-last-verify staleness signal, same
"read the cache, never run it yourself" discipline as line coverage.

## [0.12.2] - 2026-08-20

V03 write-path parity mirror (#72): `tests/parity/v03.rs`'s stub
replaced with real cases — INSERT/UPDATE/DELETE, `INSERT ... SELECT`,
`ON CONFLICT IGNORE`/`REPLACE`, `CREATE`/`DROP TABLE`/`INDEX` — driven
through the `sqlite-rs exec` CLI and diffed against the pinned
`sqlite3` oracle across the acceptance/output/schema dimensions.
`make assurance`'s Parity line moves `V03+ pending` -> `V03 3/4`.
Along the way, confirmed a known limitation (from #207) applies more
broadly than documented: an inline column-level `UNIQUE` constraint in
`CREATE TABLE` creates no backing index either (not just a composite
table-level constraint) — `compile_create_table` never auto-creates
one, so UNIQUE enforcement only fires via an explicit
`CREATE UNIQUE INDEX`. Filed as a follow-up, not fixed here.

`sqlite-rs --version`/`-V`: reports `CARGO_PKG_VERSION` and exits 0 —
the CLI previously had no way to report its own version.

## [0.12.1] - 2026-08-20

UNIQUE constraints on non-rowid columns (#207, split out of #195): new
`Opcode::NoConflict` real-index seek+branch primitive
(`src/vdbe/cursor.rs`, built on `IndexCursor::seek`) fills the gap
`CursorSlot` had no read-capable real-index variant — `compile_insert`
now probes every `UNIQUE` index before writing a row and dispatches
`ON CONFLICT` (`IGNORE`/`REPLACE`/`ABORT`+`FAIL`+`ROLLBACK`) the same
way the existing rowid-PK conflict check does. A composite
`PRIMARY KEY(...)`/`UNIQUE(...)` table constraint with no backing
on-disk index still isn't enforced (this codebase doesn't auto-create
`sqlite_autoindex_*` entries yet) — a `CREATE TABLE`-side gap, not an
INSERT-codegen one.

`INSERT ... SELECT` codegen (#208, split out of #195): `compile_insert`
now drives `select.rs`'s scan/filter/project/ORDER BY/DISTINCT/LIMIT
machinery (`compile_select_scan`, factored out of `compile_select`)
with a pluggable per-row sink, feeding each projected row into the
same per-row constraint-check/write path (`compile_row`) a literal
`VALUES` row uses — full parity with plain `SELECT`, not just
scan+WHERE. `select.rs`'s scan cursor numbers are now parameterized
(`ScanCursors`) so the embedded scan never collides with the INSERT's
own target-table/index cursors. New `Opcode::Copy` (register-to-
register, #208) re-materializes a SELECT-scan register into the fresh,
contiguous register `MakeRecord` needs once reordered/subset into the
target table's schema-column order — mirrors `compile_value`'s own
"always allocate anew" contract. Also fixes a real (pre-existing,
found via this ticket's own testing) bug in `apply_affinity`: TEXT
affinity never converted a NUMERIC value to its text rendering,
leaving e.g. `INSERT INTO t(b) VALUES (1)` (b TEXT) storing a raw
integer under a TEXT-affinity column — `PRAGMA integrity_check`
correctly flagged this as `NUMERIC value in t.b`.

## [0.12.0] - 2026-08-19

V3 exit gate (#217), closing epic #161: write-path CLI surface
(#215); corpus `PRAGMA integrity_check` cross-validation centralized
into a single `assert_integrity_check_ok` oracle helper, replacing
per-file duplicates across b-tree insert/delete, index maintenance,
pager flush, and CLI write-path tests (#216). New `exec` CLI
subcommand wires `INSERT`/`UPDATE`/`DELETE` through existing codegen,
plus new `CREATE TABLE`/`DROP TABLE`/`CREATE INDEX`/`DROP INDEX`
codegen (none existed before #215). Tier 2 (WRITE CORE) stubs flip to
real tests: `t2_crud_round_trips_on_rowid_tables` (CREATE/INSERT/
UPDATE/DELETE round-trip via the CLI) and
`t2_written_file_passes_integrity_check` (stock `sqlite3`
`integrity_check`-clean on a written file), bringing
`tests/tiers/tier2.rs` to 4/4 active. Scoped `cargo-mutants` run
against the V3 write-path modules (b-tree insert/delete/index
maintenance, VDBE write-opcode dispatch) as a sanity check ahead of
release; a full-crate mutation run remains out of scope for this
phase (scoped as a V1 exit-gate deliverable, epic #5).

## [0.11.1] - 2026-08-19

Fixes from a phase-level `/review` of V3 phase 3 (#161), found only by
looking at #195/#210/#196 together: `INSERT OR REPLACE` left stale
secondary-index entries for the row it displaced (#218); `UPDATE`
never re-validated NOT NULL/CHECK constraints, letting an invalid
value propagate into secondary indexes too (#220); `INSERT` never
wired `AUTOINCREMENT` into `NewRowid`'s opt-in mechanism, so an
`AUTOINCREMENT` table silently reused rowids after deletion (#221).
Adds integration-level regression coverage: an INSERT→UPDATE→DELETE
lifecycle test against the same indexed table, and a test pinning
today's non-enforcing UNIQUE-index behavior (tracked separately, #207).

## [0.11.0] - 2026-08-19

V3 phase 3 complete (#161): write codegen + VDBE. Auto-index
maintenance on write (#196) — `INSERT`/`DELETE`/`UPDATE` codegen now
opens a write cursor per index and emits `IdxInsert`/`IdxDelete` pairs
per row, keeping secondary indexes in sync with table data. `DESC`
index columns and invalid/untrusted `sqlite_master` root pages are
rejected outright rather than silently mis-keyed or misdirected.

## [0.10.1] - 2026-08-18

Fixes from `/review` of V3 phase 2 (#188/#190/#191/#192/#193): a parser
bug and an untrusted-input handling gap, plus minor diagnostics/doc
cleanup.

### Fixed

- `opt_column_constraint()` (DDL column-constraint parsing, #192) silently
  dropped `CONSTRAINT <name>` when no recognized constraint keyword
  followed — e.g. `CREATE TABLE t (a INTEGER CONSTRAINT foo)` was accepted
  with the constraint text discarded. Now rejected as `Invalid`, matching
  `table_constraint()`'s existing behavior.
- `find_master_rootpage()` (`src/btree/master.rs`, #193) cast an `i64`
  rootpage read from a `sqlite_master` row directly to `u32` with no
  validation — a corrupted or malicious `.db` file could store an
  out-of-range/negative rootpage and get silently mapped to a different
  page. Now rejected via a new `BtreeError::InvalidRootPage`.
- `delete_master_row` fabricated `BtreeError::RowidNotFound { rowid: 0 }`
  on a by-name lookup miss, discarding the actual name. Replaced with a
  dedicated `BtreeError::MasterEntryNotFound { name }`.

### Changed

- Documented `bump_schema_cookie`'s `wrapping_add` as deliberate parity
  with stock SQLite's own cookie wraparound.
- Added a tripwire comment on the `PageSource for &T` blanket impl.

## [0.10.0] - 2026-08-18

V3 phase 2 (epic #161) complete: write-path parser (INSERT/UPDATE/DELETE,
CREATE/DROP TABLE, CREATE/DROP INDEX) plus schema cookie + `sqlite_master`
maintenance and AUTOINCREMENT tracking.

### Added

- Schema cookie + `sqlite_master`/`sqlite_sequence` write maintenance
  (#193), V3 phase 2 (epic #161). New `src/btree/master.rs`:
  `bump_schema_cookie` patches the schema cookie (header bytes 40-43) in
  place, following the offset-patch precedent `pager.rs` uses for
  page-count/freelist fields (#167's documented "no header serializer
  yet" gap); `insert_master_row`/`delete_master_row` write/remove
  `sqlite_master` rows for CREATE/DROP TABLE/INDEX via the existing
  `insert_row`/`delete_row` b-tree primitives; `ensure_sqlite_sequence_table`
  auto-creates `sqlite_sequence` on first use and `update_sequence` tracks
  each table's max rowid (monotonic — never decreases). These are write
  primitives only; wiring them into actual statement execution is VDBE
  write-opcode scope (#194). Also adds a blanket `impl<T: PageSource>
  PageSource for &T` (`src/vfs/page_source.rs`) so a `TableCursor` can
  scan a table through a shared `&Pager` reference while the same
  `Pager` is later borrowed mutably for a write.

- Parser: CREATE/DROP TABLE, CREATE/DROP INDEX (#192), V3 phase 2 (epic
  #161). `parse_create_table`/`parse_create_index`/`parse_drop_table`/
  `parse_drop_index` accept `CREATE TABLE [IF NOT EXISTS] name (columns,
  table-constraints) [WITHOUT ROWID | STRICT]`, `CREATE [UNIQUE] INDEX
  [IF NOT EXISTS] name ON table (indexed-columns) [WHERE ...]` (partial
  index), and the two `DROP` forms, mirroring the existing three-way
  accept/unsupported/invalid outcome contract. Column/table constraints
  cover NOT NULL, PRIMARY KEY [ASC|DESC] [AUTOINCREMENT], UNIQUE,
  DEFAULT, CHECK, COLLATE, and named `CONSTRAINT`s; `REFERENCES`/
  `FOREIGN KEY` are parsed then reported `Unsupported` (deferred to V8).
  New `CreateTable`/`ColumnDef`/`ColumnConstraint`/`TableConstraint`/
  `IndexedColumn`/`CreateIndex`/`DropTable`/`DropIndex` AST nodes and
  printer round-trip support; grammar's V3 DDL stub filled in with real
  detail (`indexed-column`, `COLLATE` constraint, `CONSTRAINT` prefix).
  Verified against `tests/corpus/sql/ddl/*.sql`: 464/517 CREATE TABLE and
  148/149 CREATE INDEX statements accepted.
- Parser: UPDATE statement (#190), V3 phase 2 (epic #161). `parse_update`
  accepts `UPDATE [OR REPLACE/IGNORE/ABORT/ROLLBACK/FAIL] table SET
  col=expr, ... [WHERE ...]`, including the tuple SET form
  `(col1, col2) = (expr1, expr2)` (expanded into one `Assignment` per
  column; mismatched arity is a syntax error, a subquery RHS is
  unsupported), mirroring `parse_insert`/`parse_delete`'s three-way
  accept/unsupported/invalid outcome contract (spec 002-parser). New
  `Update`/`Assignment` AST nodes reuse the existing expr parser for SET
  values and WHERE and the existing `ConflictAction` enum; `update-stmt`
  grammar entry in `.openspec/grammar/sqlite.ebnf` extended to cover the
  conflict-action clause and tuple-assignment form.
- Parser: DELETE statement (#191), V3 phase 2 (epic #161). `parse_delete`
  accepts `DELETE FROM table [WHERE ...]` (no LIMIT/ORDER BY — deferred),
  mirroring `parse_insert`'s three-way accept/unsupported/invalid outcome
  contract (spec 002-parser). New `Delete` AST node reuses the existing
  expr parser for WHERE, plus printer round-trip support.
- Parser: INSERT statement — VALUES + SELECT forms (#188), V3 phase 2
  (epic #161). `parse_insert` accepts `INSERT [OR REPLACE/IGNORE/ABORT/
  ROLLBACK/FAIL] INTO table [(cols)] (VALUES (...), ... | SELECT ... |
  DEFAULT VALUES)`, mirroring the existing SELECT recursive-descent
  parser and its three-way accept/unsupported/invalid outcome contract
  (spec 002-parser). New `Insert`/`InsertSource`/`ConflictAction` AST
  nodes and printer round-trip support; `insert-stmt` grammar entry in
  `.openspec/grammar/sqlite.ebnf` extended to cover the conflict-action
  clause and SELECT-source alternative.

## [0.9.2] - 2026-08-18

### Fixed

- `tests/tiers/tier0.rs`: `t0_wal_pending_rows_visible` and
  `t0_any_feature_bearing_file_dumps_all_rows` both read directly from
  the shared, committed `tests/corpus/fixtures/journalstates/` WAL
  fixtures, unlike `t0_hot_journal_recovers_committed_state`, which
  already copies its fixture to a scratch dir first. Since tests in one
  binary run concurrently, and the pinned-oracle shell-out in the
  "any feature bearing file" test creates a real `-shm` file when
  `sqlite3` connects to a WAL db, a sibling thread's `dump_database`
  could observe that `-shm` mid-creation and reject it as too short —
  seen once on main CI after #191 merged (unrelated to that PR's
  content, not reproducible locally). Both tests now copy their
  fixture (and `-wal`/`-journal` companion) into an isolated temp dir
  via a new `IsolatedFixture` helper, matching the hot-journal test's
  existing isolation convention. Test-only change, no `src/` impact.

Spend: small.

## [0.9.1] - 2026-08-18

### Fixed

- Index b-tree delete: `extract_max_entry` (`src/btree/index_delete.rs`)
  permanently orphaned pages whenever a predecessor swap drained a
  subtree more than one level deep (interior → interior → leaf) — only
  the outermost page was ever deallocated, leaving deeper already-empty
  pages unreachable and never returned to the freelist. Fixed by
  deallocating a subtree's pages bottom-up as each level confirms it's
  fully drained; added a depth guard (mirroring
  `descend_index_tree`'s `MAX_PAGES_VISITED` convention) since the
  recursion previously had none. Found during `/review` of #189
  (V3 phase 1 exit gate).
- Index b-tree insert: `insert_entry` (`src/btree/index_insert.rs`)
  allocated overflow pages for a large key's payload *before* checking
  for a duplicate key, leaking that overflow chain on every rejected
  duplicate insert. The duplicate check (both the interior-match and
  leaf-level cases) now runs before any overflow allocation.

Spend: small — both fixes and their regression tests together were well
under a "small" ticket's budget; found and fixed in the same session as
the `/review` that surfaced them.

## [0.9.0] - 2026-08-18

### Added

- V3 phase 1 exit gate (epic #161): the b-tree write path is fully
  shipped — pager write path + freelist (#166, #167), table and index
  b-tree insert/delete with page split/merge/collapse (#168, #169,
  #171), overflow chain write/free (#168, #173), and statement-level
  rollback journaling (#172). Every file this crate writes is opened and
  `PRAGMA integrity_check`-ed by stock `sqlite3`; round-trip write→read
  via this crate's own readers is oracle-identical. Next up: V3 phase 2
  (0.10.0), the write-path parser + schema layer (INSERT/UPDATE/DELETE,
  CREATE/DROP TABLE/INDEX, `sqlite_master` maintenance).

- Index b-tree insert/delete — same ops for index b-trees, including
  WITHOUT ROWID tables (#171), V3 phase 1 (epic #161). New
  `src/btree/index_insert.rs::insert_entry` and
  `src/btree/index_delete.rs::delete_entry` mirror the table write path
  (#168/#169) in shape but not mechanism: index interior cells carry a
  full entry (not just a routing key), so an index leaf split promotes
  its median entry into the parent (removing it from both halves, unlike
  a table leaf split's copy-and-keep divider), and a delete target found
  at interior level requires a predecessor swap
  (`delete_via_predecessor_swap`/`extract_max_entry`) rather than a
  plain routing-entry removal, to avoid discarding the live value that
  entry itself carries. `descend_index_tree` (shared by both write
  paths) checks for an exact key match at every interior level while
  descending, not just at the final leaf — needed because a duplicate or
  delete target may have been promoted to interior level by an earlier
  split. Verified against stock `sqlite3` for single-entry insert,
  bulk insert of 500 entries (forcing splits), delete-all, WITHOUT ROWID
  insert/delete, and duplicate-key rejection (spec 006-btree
  Requirements 15-17).
- Fix (found while implementing the above): `delete.rs`'s (table,
  #169) and `index_delete.rs`'s underflow cascade both had a latent bug
  where an interior page draining to zero routing entries recursed into
  `collapse_into_ancestors` as if the page itself had "emptied" —
  silently orphaning its own still-live `rightmost` subtree (the
  grandparent's handling of "child died" repoints/removes its reference
  to the collapsing page, with nothing carrying `rightmost` forward).
  Both now `splice_child` the surviving `rightmost` directly into the
  collapsing page's own slot in its parent instead of cascading further.
  Not confirmed reachable by the table write path's actual test
  parameters (an exhaustive single-delete probe over 60 rows found no
  repro there), but the index write path hit it immediately once
  interior-level values needed preserving — applying the same
  correction to both, plus a table-side regression test
  (`deleting_one_subtree_never_orphans_a_sibling_rightmost_subtree`).

- Table b-tree delete — cell delete + page merge/rebalance (#169), V3
  phase 1 (epic #161). New `src/btree/delete.rs::delete_row`: locates a
  cell by rowid and removes it via the shared page-rebuild helpers
  (promoted from `insert.rs` to `src/btree.rs` so both write paths reuse
  them). Underflow policy is a documented simplification of SQLite's
  proactive half-full-threshold sibling redistribution: a page collapses
  into its parent only once completely empty, cascading up the ancestor
  chain and, if it reaches the root, relocating the sole remaining
  child's content into the fixed root page (the reverse of
  `insert.rs::root_split`). Emptied pages are returned to the freelist
  (#167). Verified against stock `sqlite3` for single-row delete, delete-
  all (tree collapses to an empty leaf root), bulk delete of 1000 rows,
  a collapse across a leaf-split boundary, and an insert→delete→insert
  round trip that reuses freed pages (spec 006-btree Requirements 12-14).

- Table b-tree insert — cell insert + page split (#168), V3 phase 1
  (epic #161). New `src/btree/insert.rs`: rowid-ordered leaf cell insert
  (encoding rowid varint + payload + overflow chain, reusing
  `record::encode_record`/`local_payload_size`), leaf split with
  median-key propagation, cascading interior splits, and root split
  (including the page-1/`sqlite_master` root special case). Verified
  against stock `sqlite3` (`PRAGMA integrity_check` + `select`) for
  no-split, single-split, cascading/root-split, 1000-row bulk insert,
  and overflow+split scenarios (spec 006-btree Requirements 8-11).

- Statement-level journaling — rollback journal for atomicity (#172), V3
  phase 1 (epic #161), DELETE mode only (TRUNCATE/PERSIST deferred).
  `Pager::flush` now journals the pre-transaction content of every page
  it's about to overwrite before writing to the main file, syncing the
  journal first; `Pager::open` replays a detected hot journal into the
  main file (truncating back to its pre-transaction page count) instead
  of refusing to open. New `src/pager/journal.rs` mirrors stock SQLite's
  `pager.c` header layout and `pager_cksum` byte-for-byte, proven both
  directions: a real `sqlite3`-written hot journal recovers through our
  `Pager::open` (`tests/tiers/tier0.rs`), and a journal we write recovers
  through a real `sqlite3` (`tests/corpus/journal_interop_test.rs`).
  Un-ignores `tests/tiers/tier2.rs`'s `t2_statement_atomicity` and
  `t2_journal_transactions_commit_and_rollback` (spec 007-pager
  Requirement 6, ADR-0016). Spend: ~2x the initial estimate — recovery
  correctness (real sqlite3 interop, byte-for-byte checksum format, and
  making sure recovery tests never mutate checked-in fixtures) took more
  iteration than the write-path plumbing alone.

- Freelist management — allocate/deallocate pages (#167), V3 phase 1
  (epic #161). `Pager::allocate_page` pops a page off the freelist (or
  extends the file when it's empty); `Pager::deallocate_page` pushes a
  page onto the freelist, appending to the current trunk page's leaf
  array or chaining a new trunk once it's full. New
  `src/pager/freelist.rs::TrunkPage` parses/writes freelist trunk pages,
  never panicking on a truncated/corrupt trunk. An allocate/deallocate
  round trip still opens and `PRAGMA integrity_check`s cleanly in stock
  `sqlite3` (spec 007-pager Requirement 5).

- Pager write path — dirty page tracking + flush (#166), V3 phase 1
  (epic #161). `Pager::get_page_mut`/`Pager::flush` on top of a new
  `Vfs::open_write`/`VfsFile::write_at`/`sync` surface (implemented for
  both `UnixVfs` and `MemoryVfs`). `Pager` now holds a single read-write
  file handle instead of opening a second fd, avoiding the documented
  `close()`-drops-all-`fcntl`-locks hazard. A page flushed through the
  new write path still opens and `PRAGMA integrity_check`s cleanly in
  stock `sqlite3` (spec 007-pager Requirement 4).

### Fixed

- Overflow chain pages leaked on b-tree row delete (#173), V3 phase 1
  (epic #161). `delete_row` (#169) freed emptied leaf/interior pages but
  never freed the overflow pages a deleted cell's payload had spilled
  into (#168) — those pages were orphaned instead of returning to the
  freelist (#167). `src/btree/delete.rs` now reads the removed cell's
  first overflow pointer and walks/deallocates the whole chain, with the
  same revisited-page cycle guard the read-side `reassemble_payload`
  uses. #173 was re-scoped to this narrower gap once investigation found
  the insert-side overflow-chain write was already delivered by #168.

## [0.8.0] - 2026-08-16

### Added

- V2 exit gate (#97): closes epic #56 — V2, single-table queries (Tier 1
  QUERY CORE), is fully shipped across all four phases (tokenizer +
  SELECT-core parser, value-semantics kernel + scalar function core,
  VDBE interpreter, `sqlite-rs query` CLI).
- `tests/tiers/tier1.rs::t1_select_core_accepts_and_rejects` — the last
  Tier 1 stub, flipped live: accept/reject vectors for the SELECT-core
  parser's three-way `ParseOutcome` contract (spec 002-parser
  Requirement 4). Tier 1 is now 7/7 active, no ignores.

### Fixed

- `tools/assurance.py`'s opcode-completeness scan missed dispatch arms
  combining multiple opcodes (`SorterSort | Sort => ...`), undercounting
  `Sort`/`SorterSort` as unimplemented. Opcode completeness now
  correctly reads 64/64 against the frozen inventory (#65/#87) — both
  opcodes were already dispatched.
- `.openspec/specs/001-architecture/spec.md`: removed a stale
  `(planned)` dead link on Requirement 1 (a real test link already
  covers the scenario) and repointed Requirement 4's dead link to the
  actual tier0 test (`tests/tiers/tier0.rs::t0_feature_bearing_files_are_raw_row_readable`).

## [0.7.8] - 2026-08-16

### Added

- Codegen pattern-matches `WHERE rowid = <int literal>` / `WHERE rowid =
  ?` (or `?NNN`) — recognized via the `rowid`/`_rowid_`/`oid` keywords or
  the table's actual `INTEGER PRIMARY KEY` alias column — and emits
  `Integer`/`Variable` + `SeekRowid` directly on the table cursor instead
  of the `Rewind`/`Next` full-table-scan loop: an O(log n) point lookup
  instead of an O(n) scan (#137). Making `WHERE rowid = ?` actually
  correct (not just compile) required reopening the frozen V2 opcode set
  to add `Variable` (re-harvested from the pinned oracle) plus a minimal
  bind-parameter API — `Vm::bind_params`, `execute_with_params`/
  `execute_with_db_and_params` — see `.openspec/adr/0015-variable-opcode-reopens-frozen-set.md`.
- `tests/performance/point_lookup.rs`: a quick, dependency-free wall-clock
  demonstration of the O(n)→O(log n) fix (`make test-point-lookup-perf`),
  plus a small `tests/performance/Makefile` to run individual
  test/bench scenarios standalone.

## [0.7.7] - 2026-08-16

### Added

- `ORDER BY` by a genuine computed expression — unary/binary operators,
  scalar function calls, and an alias whose own result expression is
  computed rather than a bare column (#155). `compile_sorted_scan`
  computes each such term into its own register, appended after the
  raw schema-column block already fed to `MakeRecord`/`SorterInsert`;
  the `SorterOpen` sort-key descriptor is patched in once that layout
  is known (new `Emitter::patch_p4`), and the record's span is widened
  to the register allocator's post-compile watermark (new
  `RegAlloc::peek`) so expressions with internal temporaries (e.g.
  `CASE`) stay record-contiguous. Closes the gap #144 left open.

## [0.7.6] - 2026-08-16

### Fixed

- Literal fidelity: REAL/BLOB literals compiled to `String8` text instead
  of typed values, integers outside `i32` were a hard codegen error, and
  `CAST` misused `MustBeInt`/`RealAffinity` (aborting instead of
  truncating, leaving TEXT/BLOB/NUMERIC targets as no-ops) (#142).
  Harvests `Real`, `Blob`, `Int64`, `Cast` from the pinned oracle and adds
  `src/vdbe/cast.rs`, a kernel module implementing SQLite's real `CAST`
  conversion rule (longest-numeric-prefix parsing, saturating
  truncation, the NUMERIC whole-number downgrade that applies only to
  text/blob sources). Also fixes a `%` bug this exposed: `checked_rem`
  always returned `Integer`, but SQLite promotes to `REAL` when either
  operand is `REAL`. 24 new oracle-harvested CAST vectors added to
  `tests/corpus/expr_vectors/walker.jsonl`; supersedes the narrower
  BLOB-only follow-up filed as #151.

## [0.7.5] - 2026-08-16

### Added

- `ORDER BY` resolves 1-based ordinals (`ORDER BY 2`) and result-column
  aliases (`ORDER BY x`), not just bare table-column references (#144).
  Aliases take precedence over table columns, matching SQLite.
  `ORDER BY ... COLLATE name` now reads the actual collation instead of
  always comparing under BINARY. Both resolve to the same underlying
  table-column index a bare column reference already used, so no
  sorter/`SortKeyColumn` change was needed. Genuine expression sort keys
  (`ORDER BY -i`, `ORDER BY lower(s)`) still refuse — extending the
  sorter's record payload for computed values is tracked separately
  (#155).

## [0.7.4] - 2026-08-16

### Fixed

- `query` `-list`-mode rendering diverged from `sqlite3 -list` on NULL,
  BLOB, and REAL (#143): `query`'s default output reused `dump`'s
  `.dump`-`quote()`-style renderer (`NULL` literal, `X'HEX'` blobs)
  instead of the shell's actual `-list` rules (empty string for NULL,
  raw blob bytes, truncated at the first embedded NUL byte since the
  shell prints via a null-terminated C string). A dedicated byte-based
  `format_query_value` renderer now backs `query`'s `-list` branch;
  `dump`/`export` are unchanged.
- REAL columns storing exactly `0.0`/`1.0` (SQLite's integer-serial-type
  storage optimization) decoded as a bare `Integer` and rendered `0`
  instead of `0.0` for *any* reader, not just `query` — `emit_column_read`
  never applied REAL-affinity coercion on column reads. `apply_affinity`
  now converts `Integer -> Real` for REAL affinity (matching SQLite's
  documented affinity rule), and `emit_column_read` emits `RealAffinity`
  after `Column` for REAL-affinity columns.

## [0.7.3] - 2026-08-16

### Fixed

- Quote-aware DDL column splitting (#135, follow-up from #131 review):
  `column_defs`/`split_top_level_commas` split a `CREATE TABLE` column
  list on raw top-level commas with no awareness of string literals,
  quoted identifiers, or comments, and `rowid_alias_column` scanned that
  raw text for `PRIMARY KEY`/`INTEGER` — since #131 this drives
  `emit_column_read`'s `Rowid`-vs-`Column` choice, so a mis-split was a
  silently wrong query result, not just wrong `dump` output. A new
  length-preserving `mask_quotes_and_comments` blanks out `'...'`,
  `"..."`, `` `...` ``, `[...]`, `--...`, and `/*...*/` regions before
  paren-depth/comma-splitting and keyword scanning, while the returned
  text still slices the original string. `rowid_alias_column` also now
  recognizes the table-level `PRIMARY KEY(col)` constraint form,
  previously dropped by the table-constraint filter before it was ever
  checked.

## [0.7.2] - 2026-08-16

### Fixed

- Comparison affinity was never applied — `WHERE i = '5'`, `WHERE i > 3`,
  `WHERE r = 1.5` returned no rows instead of matching the oracle (#138).
  `TableSchema` now captures each column's declared type; codegen derives
  comparison affinity from both operands (columns/CASTs only, per SQLite's
  own `comparisonAffinity` rule) instead of hardcoding the P4 affinity
  byte, and `compare_jump` applies it to operand copies before delegating
  to `compare()`. Spec 009 Req 5 gains a scenario for the affinity half of
  the P4 descriptor. Known remaining gap, filed separately (#151): BLOB
  literals still compile to text, so `WHERE b = x'41'` doesn't match yet.

## [0.7.1] - 2026-08-16

### Added

- Performance regime, first results (#112, epic #111): tier-1
  (engine-to-engine, criterion) and tier-2 (CLI-to-CLI, hyperfine) bench
  harnesses against the pinned 3.53.4 oracle. `tools/gen_fixtures.sh --bench`
  generates ~1MB/~50MB fixtures (pure-SQL, deterministic, not committed);
  `tests/performance/engine.rs` runs 6 scenarios per fixture with rusqlite
  linked to the pinned oracle (not its `bundled` feature, so it can't drift);
  `tools/bench_cli.sh` compares `sqlite-rs dump`/`query` against `sqlite3`;
  `tools/bench-status.json` is the committed first-results table. `make
  bench`/`make bench-cli`/`make bench-status`/`make fixtures-bench`.
  Deliberately not wired into CI — `make lint` scopes clippy to `--lib --bins
  --tests --examples` rather than `--all-targets` so benching stays a manual
  workflow. Findings: full scan/filter/expr/prepare land in the expected
  1.5–6× band; point lookup and `ORDER BY ... LIMIT` are 500×–41,000× outliers
  (full scan instead of a rowid seek / no top-K bound — V4 planner-tuning
  material, filed as #128/#129, not fixed here).
- Phase 4B (#96, epic #56): sqllogictest slice runner — `tests/sqllogictest/`
  parses the sqllogictest record format (`statement ok/error`,
  `query <types> <sort>`, `----` expected blocks with literal values or the
  `N values hashing to <md5>` form, `onlyif`/`skipif` engine conditionals) and
  runs the 14 vendored files (#70) through the same read pipeline
  `sqlite-rs query` uses. `statement ok` setup replays through the pinned
  oracle, since this engine has no write path yet.
- Skip-not-fail policy per spec 004 Req 4: out-of-slice grammar/opcode gaps
  skip, only a genuine result divergence fails. `make sqllogictest` +
  informational (non-gating) CI step, plus a companion step that reports
  drift between the committed status file and what a run produces.
- `tools/sqllogictest-status.json`: committed pass/skip/suspect/fail counts,
  reported on the assurance dashboard's Model line as a pass-rate AND
  coverage pair — currently 199/199 passing over 8.6% of the corpus. The
  `suspect` bucket counts queries declined for reasons that should not occur
  against oracle-validated input (malformed-SQL verdict, unreadable schema),
  so an engine regression there surfaces instead of hiding among the skips.
- `tests/unit/codegen.rs`: oracle-free program-shape tests pinning each
  codegen fix below, so a regression fails `make test` rather than only the
  non-gating slice.
- `tests/codegen/expr_test.rs`: two end-to-end regression tests (#125/#133)
  for the scalar-function contiguity fix below — `single_arg_function_call_compiles`
  and `multi_arg_function_call_compiles_with_contiguous_registers` compile
  real SQL through the full parse → codegen → VDBE path and assert on actual
  output values, complementing the program-shape tests above.
- Two opcodes join the frozen V2 set, taking it from 52 to 54 (#134):
  `Not` (`r[P2] = !r[P1]`, NULL in / NULL out) and `Null` (writes NULL
  over the register range `P2..=P3`). Both were harvested, not
  hand-added — `tools/harvest_opcodes.py` gained the two oracle queries
  that emit them (`SELECT NOT qty FROM products`,
  `SELECT CASE WHEN price > 100 THEN 1 END FROM products`), so
  `make opcodes` reproduces the inventory. Opcode completeness moves
  50/52 → 52/54; both are dispatched on arrival.
- `tests/parity/v02.rs`: a three-valued-logic parity dimension over
  `serialtypes/values.db` (the fixture that actually has NULL rows) —
  14 cases covering `NOT` over every comparison, connective, `IN`,
  `BETWEEN`, and `IS NULL` form, plus value-context cases wrapped in
  `IS NULL` so the assertion is about semantics rather than about how
  each engine spells a null.

### Fixed

Codegen defects the runner and its review surfaced, all affecting
`sqlite-rs query` output (#95's shipped CLI), not just tests:

- `x NOT IN (...)` and `x NOT BETWEEN a AND b` returned rows for NULL
  operands. Both were compiled as their positive form with true/false jump
  targets swapped, which turns SQL's "unknown" into "true"; they now lower
  the way SQLite does (`NOT BETWEEN` as `x < lo OR x > hi`, `NOT IN` with an
  explicit saw-NULL guard). The generic `NOT (...)` case this left open is
  fixed by #134, below.
- Every scalar function call with arguments failed to compile
  (`function argument registers were not contiguous`), making V2's scalar
  functions unreachable through the compiled query path — `SELECT abs(id)`
  included. `Function`'s contiguous argument window was reserved *before*
  the arguments were compiled, so they always landed past it; the window is
  now taken from where the arguments actually land. Slice coverage rose from
  7.2% to 8.6% as a result.
- Generic `NOT (...)` resolved SQL's "unknown" to true, so
  `WHERE NOT (x = 5)` returned rows where `x IS NULL`, and the two
  spellings `NOT (x IN (...))` and `x NOT IN (...)` disagreed (#134).
  `compile_cond` now carries a third piece of contract alongside its
  true/false continuations — `NullTarget`, which names the one the
  *unknown* outcome joins, exactly SQLite's `jumpIfNull` flag. `NOT`
  swaps the two targets and flips it, leaving unknown on the address it
  already had; `AND`/`OR`/`BETWEEN`/`IN` thread it through unchanged.
  The same flip fixes `x <> 5`, which had the identical bug for the
  identical reason (`<>` is `Eq` with the targets exchanged) and also
  returned NULL rows.
- Conditions used as values (`SELECT x = 5`, `SELECT a AND b`) answered
  NULL for every row — they fell into `compile_value`'s catch-all,
  which allocates an unwritten register. They now materialize all three
  outcomes, and `SELECT NOT x` yields NULL for a NULL `x` instead of 1.
  `CASE ... ELSE NULL` leaked the previous row's result, since its NULL
  branch emitted no instruction at all to overwrite the shared
  destination register.
- `SELECT *` (and `SELECT tbl.*`) answered NULL for an
  `INTEGER PRIMARY KEY` column. #131 routed the WHERE and named-result-
  column paths through `emit_column_read`, which substitutes `Rowid`
  for the record's NULL placeholder, but the star-expansion path in
  `compile_row_values` emitted its own bare `Column` and was missed —
  so `SELECT id FROM t` was right while `SELECT * FROM t` was wrong.
  No corpus fixture is a plain table with an `INTEGER PRIMARY KEY`,
  which is why the oracle suites could not see it; the new parity case
  borrows an FTS5 shadow table until the corpus gains one.
- `tests/codegen/expr_test.rs`'s walker-vector reader extracted JSON
  string fields by splitting on the next `"`, without unescaping. That
  silently changed the SQL under test: `'a%b' LIKE 'a\\%b' ESCAPE '\\'`
  reached the compiler with a two-character escape, which SQLite itself
  rejects. The resulting failure had been filed against `Like`'s
  codegen — the engine agreed with the oracle all along. Both `ESCAPE`
  vectors leave `KNOWN_GAPS`, and the pass ratchet moves 44 -> 46.
- Aggregate calls (`count`, `sum`, `avg`, ...) compiled as ordinary per-row
  scalar functions, so `SELECT count(*) FROM t` emitted one row per input row
  instead of one count. Codegen now rejects them as unsupported — V2 has no
  grouping pass — since a refusal beats silently wrong output. In slice terms
  this is a boundary re-label rather than a repair: those queries move from
  the fail column to the skip column, which is why the metric publishes
  coverage alongside pass rate.
- A rowid-alias column (`INTEGER PRIMARY KEY`) read back as NULL, because it
  is stored as a placeholder in every record and needs the cursor's rowid
  substituted. `SELECT x FROM t WHERE x=2` silently matched nothing.
  `rowid_alias_column` moved from `src/dump.rs` to `src/schema/` so the
  compiled read path can share the substitution `dump` already did. Its
  detection now also excludes `INTEGER PRIMARY KEY DESC`, which SQLite
  deliberately does not treat as a rowid alias; two remaining textual
  misreads are pinned by `known_fragile_*` tests.

- `&`, `|`, `<<`, `>>`, `~`, and `||` all parsed (in-grammar since V2) but
  silently answered NULL (binary ops fell into `compile_value`'s catch-all,
  which emits a bare `Null`) or passed the operand through unchanged
  (`~`, `UnaryOp::BitNot`) (#139). Harvested the six real SQLite opcodes
  `BitAnd`/`BitOr`/`ShiftLeft`/`ShiftRight`/`BitNot`/`Concat` (54 -> 60
  opcodes), added their INTEGER/TEXT coercion and NULL propagation to
  `src/vdbe/coerce.rs`, and wired dispatch through `src/vdbe/arithmetic.rs`.
  Shift handles SQLite's negative-shift-amount reversal and
  magnitude-≥64 clamp rules, not just Rust's native `<<`/`>>`. Six
  `KNOWN_GAPS` entries close; the walker-vector pass ratchet moves 46 -> 55.

VDBE execution limit, unrelated to codegen, surfaced by #112's bench:

- `src/vdbe/exec.rs`'s `MAX_STEPS` infinite-loop backstop was `1_000_000`,
  which a real ~830k-row full-table scan already exceeds (a handful of
  VDBE steps per row). Raised to `50_000_000` — still a bounded safety net,
  now sized for real workloads instead of only small test fixtures.
- `ORDER BY ... NULLS FIRST/LAST` (#140) was parsed and stored
  (`ast::OrderingTerm::nulls_last`) but never read by `resolve_order_by`,
  so an explicit modifier was silently ignored — the sorter always placed
  NULLs first for ASC / last for DESC regardless of the clause. `SortKeyColumn`
  now carries `nulls_first`, derived from the parsed term (defaulting to
  the prior implicit ASC/DESC-driven placement when no clause is given),
  and the sorter compares NULL-vs-non-NULL independently of the
  `descending` reversal. Declared-column collation in ORDER BY remains
  hardcoded to `Collation::Binary` — schema has no per-column collation
  storage yet — tracked as a follow-up, not fixed here.

## [0.7.0] - 2026-08-16

### Added

- Phase 3C (#91, epic #56): the codegen convergence ticket — `src/codegen/`
  (`select.rs`, `expr.rs`) compiles a parsed `Select` AST into a VDBE
  `Program`: full-table scan (`Init -> OpenRead -> Rewind -> ... -> Next
  -> Halt`), WHERE/AND/OR/CASE/BETWEEN/IN/LIKE as jump-based control flow
  (never an intermediate boolean register, per spec 009 Req 11), ORDER BY
  via the sorter, LIMIT/OFFSET counters, DISTINCT via the ephemeral index.
- Wires the previously-missing `Function` opcode dispatch in
  `src/vdbe/exec.rs` (spec 009 Req 7).
- `src/vdbe/explain.rs`: the `EXPLAIN` bytecode printer (spec 009 Req 10).
- Flips spec 009 Requirements 7/10/11 from `(planned)` to active — all 11
  requirements now backed, zero dead links.
- Un-ignores `tests/tiers/tier1.rs`'s `t1_single_table_where_matches_oracle`
  and `t1_explain_prints_bytecode`.

Known scope gaps (documented via `KNOWN_GAPS` in the test files, and now
erroring loudly rather than silently corrupting, per PR #117 review):
no bitwise/concat opcode in the frozen V2 52-opcode set; CAST's
lossy-conversion semantics beyond affinity coercion; REAL literals
represented as text (no `OP_Real`-equivalent opcode); integer literals
outside `i32` range; CASE branch results other than a bare literal or
column reference (no MOVE opcode); full three-valued NULL propagation
through `NOT`/`AND`/`OR`/`BETWEEN`/`IN` in value (non-WHERE) context.

**0.7.0 completes V2 phase 3** (#87, #88, #89, #90, #91 all closed) —
epic #56's engine phase: VDBE interpreter, cursor/sorter/ephemeral
opcodes, and now codegen + EXPLAIN, all oracle-parity-tested end to end
against the V2 query corpus.

Spend: estimated Large on take (#91 had no prior complexity estimate);
actual spend matched, including a follow-up fix pass for PR #117's
review findings (reachable panics, a silent CASE data leak, and the
project's `mvl-limit` qualified-subset CI gate).

## [0.6.7] - 2026-08-16

### Fixed

- `parse_select` was reporting several syntactically-valid-but-unimplemented
  SELECT constructs as `ParseOutcome::Invalid` (genuine syntax error)
  instead of `Unsupported` (#110): `IN <table-name>`, bare `VALUES` /
  compound, `HAVING` without `GROUP BY`, `NOT INDEXED`, schema-qualified
  table names (`aux.t5`), `->`/`->>` operators, and
  `OUTER LEFT NATURAL JOIN`. This matters for #96's slice-boundary
  triage, which otherwise misreads these as our-bug.
- Along the way, found and fixed a pre-existing dead-code bug: the
  subquery-in-FROM `Unsupported` branch in `table_ref()` was
  unreachable because `identifier()` was called before the `(` check.
- Spend: on track vs #110's ~120k token estimate.

## [0.6.6] - 2026-08-16

### Added

- Phase 3B (#90, epic #56): the cursor, ephemeral-index (DISTINCT), and
  sorter (ORDER BY) VDBE opcode families on top of #89's core —
  `src/vdbe/cursor.rs` (`OpenRead`/`OpenEphemeral`/`OpenPseudo`/
  `Rewind`/`Last`/`Next`/`Column`/`Rowid`/`SeekRowid`/`NullRow`/
  `Sequence`/`Found`/`IdxInsert`/`IdxLE`/`Delete`) and
  `src/vdbe/sorter.rs` (`SorterOpen`/`SorterInsert`/`SorterSort`/`Sort`/
  `SorterNext`/`SorterData`), keyed by a new `P4::SortKey`/
  `SortKeyColumn` descriptor.
- `TableCursor::last()`/`prev()` (`src/btree.rs`) — reverse b-tree
  traversal, mirroring `first()`/`next()`.
- `Vm::with_db`/`execute_with_db` — attaches a shared page source so
  `OpenRead` can open real cursors, alongside the existing
  register-only `execute()`.
- Hand-assembled acceptance programs (`tests/vdbe/cursor_sorter_test.rs`)
  reproduce full-scan, ORDER BY, and DISTINCT against real corpus
  fixtures. Spec 009's Requirements 4 (cursor) and 9 (sorter) flip from
  `(planned)` to active.
- Spend: on track vs #90's estimate (large, ~2500-3500 lines).

## [0.6.5] - 2026-08-16

### Added

- Wired `tools/opcodes-v2.json` (the oracle-harvested 52-opcode set,
  #58) into `tools/assurance.py` as a VDBE completeness checklist (#65),
  now that phase 3A (#89) landed a real dispatch table to count
  against: an `Opcode completeness:` line in the Model section reports
  how many opcodes `src/vdbe/exec.rs`'s `dispatch` actually handles
  versus the harvested total (30/52 as phase 3A lands).
  `Opcode::ALL` (`src/vdbe/program.rs`) plus
  `tests/vdbe/opcode_completeness_test.rs` keep the enum and the
  harvested set from drifting apart silently.

## [0.6.4] - 2026-08-16

### Added

- Test coverage raised on every file that `make coverage` flagged below
  85% line coverage: `vdbe/value.rs` (80.7% → 100%, `sql_lt` was
  entirely untested), `vfs/page_source.rs` (76.0% → 97.78%, page-zero
  and short-read error paths), `vdbe/coerce.rs` (83.5% → 94.87%,
  `checked_sub` was entirely untested plus the Real-operand arithmetic
  path), `vdbe/functions.rs` (70.3% → 89.96%, `nullif`/`sign`/`instr`/
  `trim`/`ltrim`/`rtrim`/`replace` were registered but never invoked by
  any test), and `parser/printer.rs` (64.47% → 98.48%, its
  `test_roundtrip_fixpoint` corpus expanded from 9 to ~40 SQL strings
  covering the AST's full print surface — DISTINCT/ALL, table aliases,
  qualified columns, all unary/remaining binary operators, every
  literal and param kind, LIKE/GLOB/ESCAPE, COLLATE, CASE variants).
  `parser/grammar.rs` also moved 82.4% → 91.46% as a side effect.
  Every file in the project is now ≥85%; TOTAL 89.11% → 94.00%. Spend:
  small, matched estimate.

## [0.6.3] - 2026-08-16

### Added

- **`src/vdbe/functions.rs`**: `like`/`glob` scalar functions (spec
  `008-value-semantics` Req 6, #59). Spec 009 Req 7 (#88) dispatches
  `like(2)` through the `Function` opcode into spec 008's registry, but
  the registry had no `like`/`glob` — this closes that gap, so no
  LIKE-specific VDBE logic is needed. ASCII case-insensitive `%`/`_`
  matching with `ESCAPE` for `like`; case-sensitive `*`/`?`/`[...]`
  (incl. `[^...]` negation and `-` ranges) for `glob`. Note SQLite's
  reversed argument order: `like(pattern, text[, escape])`.
- **`tests/corpus/expr_vectors/walker.jsonl`**: 71 oracle vectors
  covering CASE/CAST/LIKE/GLOB/BETWEEN/IN-list/short-circuit/arithmetic,
  harvested by spike 008 (#59) as phase-3 acceptance material.

### Fixed

- **`src/parser/grammar.rs`**: keyword-named function calls
  (`replace(...)`, `glob(...)`) were rejected — `REPLACE` tokenizes as a
  keyword, not an identifier, but SQLite accepts most keywords as
  function names when followed by `(`. This had silently blocked the
  `functions.jsonl` corpus (committed since #78/#79) from ever being
  executed. Found by spike 008 (#59).
- **`src/parser/grammar.rs`**: `-9223372036854775808` now parses as
  `Literal::Integer(i64::MIN)` rather than a REAL — the tokenizer folds
  the positive form to a Float since it has no i64 representation.

## [0.6.2] - 2026-08-16

### Fixed

- **`tests/tiers/tier1.rs`**: flipped the `t1_expression_kernel_affinity_and_collation_vectors`
  stub, un-ignored since #78 (value-semantics kernel) shipped in 0.6.0 but
  was never flipped — a tier-stub-flip process gap caught by a parity
  review. Mirrors the sibling `t1_scalar_functions_match_oracle` pattern: a
  light direct-API smoke test over `affinity_of`/`apply_affinity`/`compare`/
  `compare_text`, with full oracle-vector coverage remaining in
  `expr_vectors_test.rs`. Spend: trivial.

### Docs

- **`CLAUDE.md`**: added an "Epic & phase breakdown conventions" section
  documenting the `V{N} phase {M}[{letter}]` ticket-naming and
  one-minor-per-completed-phase versioning pattern already in use on epic
  #56, so future epics (V3+) follow it consistently instead of
  re-deriving it each time.

## [0.6.1] - 2026-08-15

### Fixed

- **`src/vdbe/functions.rs`**: robustness gaps found in #92's review, #99.
  `zeroblob()` now clamps its requested length to `MAX_BLOB_LEN` (1e9)
  instead of allocating an unbounded amount — a huge `N` previously hit
  Rust's allocator abort path. `iif()`'s TEXT-condition truthiness now
  checks both `Integer(0)` and `Real(0.0)` coercion outcomes (`'0.0'`
  was incorrectly truthy). `round()` clamps `digits` to SQLite's `[0,
  30]` range and propagates NULL when the digits argument is NULL
  instead of silently treating it as 0. Spend: small, matched the
  review-fix estimate.

## [0.6.0] - 2026-08-15 — V2 phase 2 complete

### Added

- **`src/vdbe/`**: the value-semantics kernel — spec `008-value-semantics`
  Requirements 1-5, #78. `affinity.rs` (5-way type affinity derivation +
  application), `compare.rs` (cross-type comparison order, NULL <
  numeric < text < blob, with SQLite's exact `i64`/`f64` boundary
  comparison), `collation.rs` (BINARY/NOCASE/RTRIM), `coerce.rs`
  (longest-valid-numeric-prefix text coercion, checked arithmetic with
  REAL-overflow promotion), `value.rs` (NULL propagation, three-valued
  `AND`/`OR`/`NOT`, `IS`/`IS NOT`). Pure functions on `Value`, no parser
  or VDBE-evaluator coupling — runs parallel to the #61 parser work.
  Spend: matched the medium estimate. Fuzz/proptest coverage deferred
  to #85.
- **`tests/fuzz/fuzz_targets/semantics_compare.rs`**, **`tests/semantics_proptest.rs`**:
  fuzz + proptest coverage for the value-semantics kernel (#78 follow-up,
  #85), spec `008-value-semantics` Requirements 1, 2, 5 — `compare`
  antisymmetry/transitivity/never-panics across arbitrary `Value` pairs
  and collations, `apply_affinity` idempotence, `coerce_text_to_numeric`
  idempotence on numeric text. Spend: matched the Small (~100k) estimate.
- **`src/vdbe/functions.rs`**: the V2 scalar function set — spec
  `008-value-semantics` Requirement 6, #79. `length`, `upper`/`lower`,
  `substr` (a faithful port of SQLite's `substrFunc` index arithmetic),
  `abs`, `coalesce`/`ifnull`/`nullif`, `typeof`, `hex`/`unhex`, `quote`,
  scalar `min`/`max`, `round`, `sign`, `instr`, `trim`/`ltrim`/`rtrim`,
  `replace`, `zeroblob`, `iif` — pure `fn(&[Value]) -> Result<Value,
  FunctionError>`, dispatched through a case-insensitive name+arity
  registry (`call_function`), ready for phase 3's `Function` opcode.
  Known gap: `quote()`'s REAL rendering doesn't byte-exact-match
  SQLite's own (observably build-dependent) higher-precision routine —
  same divergence already scoped out of `.dump`/`-list` in #37. Spend:
  matched the large estimate.

This closes out V2 phase 2 (value semantics + scalar functions) — next
up is V2 phase 3 (single-table SELECT execution, the `Function` opcode).

## [0.5.4] - 2026-08-15 — value-semantics kernel fuzz/proptest coverage

### Added

- **`tests/fuzz/fuzz_targets/semantics_compare.rs`**, **`tests/semantics_proptest.rs`**:
  fuzz + proptest coverage for the value-semantics kernel (#78 follow-up,
  #85), spec `008-value-semantics` Requirements 1, 2, 5 — `compare`
  antisymmetry/transitivity/never-panics across arbitrary `Value` pairs
  and collations, `apply_affinity` idempotence, `coerce_text_to_numeric`
  idempotence on numeric text. Spend: matched the Small (~100k) estimate.

## [0.5.3] - 2026-08-15 — `-shm` length hardening

### Fixed

- **`src/vfs/shm.rs`**: bounded `-shm` file length against oversized
  files (#54). #66 had already eliminated the `SIGBUS` risk #54 was
  filed for by switching `-shm` access from `mmap` to `pread`/`pwrite`;
  this closes the remaining gaps — an upper bound in `validate_shm_len`
  and a regression test — and records the pread/pwrite decision in
  `.openspec/adr/0001-shm-access-pread-not-mmap.md`.

## [0.5.2] - 2026-08-15 — V2 phase 1: SELECT-core parser

Hand-written recursive-descent parser + typed AST for the SELECT-core V2
slice, spec `002-parser` Requirements 2-4, #61. Spend: matched the 1.2M
"Large" estimate.

### Added

- **`src/parser/ast.rs`**: typed AST for `SELECT [DISTINCT] ... FROM
  table [WHERE] [ORDER BY] [LIMIT [OFFSET]]` and its full V2 expression
  grammar (literals, params, column refs, function calls, unary/binary
  ops at SQLite precedence, `IS [NOT] NULL`, `[NOT] BETWEEN`, `[NOT] IN`,
  `[NOT] LIKE/GLOB [ESCAPE]`, `CASE`, `CAST`, `COLLATE`, parens). Every
  node carries a `Span`; parenthesization is preserved explicitly via
  `ExprKind::Paren`.
- **`src/parser/grammar.rs`**: the recursive-descent parser itself, one
  method per precedence level mirroring `parse.y`'s `%left`/`%right`
  table exactly. Recursive-descent entry points (`expr`/`not_expr`/
  `unary_expr`) are depth-guarded (`MAX_EXPR_DEPTH`) so pathological
  nesting fails cleanly instead of overflowing the stack.
- **`src/parser/error.rs`**: `parse_select` / `ParseOutcome` — the
  three-way accept / reject-unsupported / reject-invalid outcome from
  spike 006 (#57). Unsupported-but-valid constructs (JOIN, GROUP BY,
  compound SELECT, subqueries, CTEs, window functions) are distinguished
  from genuine syntax errors, each pointing at the triggering token.
- **`src/parser/printer.rs`**: `Display` roundtrip printer, verified as a
  parse -> print -> parse fixpoint.
- `tests/unit/parser.rs`: 32 unit tests covering the full V2 grammar,
  both diagnostic outcomes, the roundtrip fixpoint, and deeply-nested
  pathological input.
- `tests/corpus/parser_oracle_test.rs`: accept/reject-unsupported/
  reject-invalid parity against a live `sqlite3` oracle across the V2
  corpus slice — the ticket's "oracle parity" acceptance bar.
- `tests/fuzz/fuzz_targets/parse_select.rs` (`make fuzz-parse-select`):
  fuzz target asserting `parse_select` never panics.

### Changed

- `.openspec/specs/002-parser/spec.md`: Requirements 2-4 flip
  `(planned)` → active, all in-scope V2 scenarios test-linked (CTE and
  window-function scenarios stay `(planned)` — V4/V9 per the grammar's
  future-blocks stubs).

## [0.5.1] - 2026-08-15 — VFS unsafe elimination

### Changed

- **`src/vfs/` no longer needs `unsafe`** (#66): `src/vfs/lock.rs`'s raw `libc::fcntl(F_SETLK)` is now `nix::fcntl::fcntl` (a safe wrapper); `src/vfs/shm.rs` no longer `mmap`s the `-shm` file — `aReadMark`/`mxFrame` access is `std::os::unix::fs::FileExt::{read_at, write_at}` (`pread`/`pwrite`) at the same fixed offsets, and SHM lock slots use the same safe `fcntl` wrapper. `src/lib.rs` is `#![forbid(unsafe_code)]` crate-wide again, with no local override anywhere in the crate. `libc` is no longer a direct dependency; `nix` (features: `fs`) replaces it.
- Cross-process lock/shm tests now spawn a genuine subprocess (`tests/helpers/lock_probe.rs`, a `[[bin]]` target) via `std::process::Command`, instead of `fork`/`waitpid`/`_exit` — a fresh address space, closer to a real second `sqlite3` process, and needs no `unsafe`.
- `Makefile`'s `mvl-limit` `src/vfs/*` exclusion rationale is now `dyn` only (the `Vfs`/`VfsFile`/`SharedLockGuard` trait objects) — the `unsafe` rationale no longer applies.

### Fixed

- The `-shm` `SIGBUS` known limitation (below, from 0.3.0) is gone: without an `mmap`, a `-shm` file truncated out from under a reader now yields a structured `Err` from the failing `read_at`/`write_at`, not an uncatchable process kill. Coherence between this crate's buffered `pread`/`pwrite` access and a concurrent `sqlite3` process's own `MAP_SHARED` mapping of the same file relies on the OS's unified page cache — true on Linux and macOS, sqlite-rs's supported platforms.

## [0.5.0] - 2026-08-15 — V2 phase 1: tokenizer

`src/parser/tokenizer.rs` — a complete SQL tokenizer, spec `002-parser` Requirement 1, #60.

### Added

- **`src/parser/tokenizer.rs`**: `Token`/`Span`/`TokenKind`/`Keyword`/`Param` types and the scanner. Covers all 146 SQLite reserved keywords (case-insensitive), bare/quoted/bracketed/backticked identifiers, integer/hex/float/string/blob/`NULL`/`TRUE`/`FALSE` literals, all operators/punctuation (incl. `||`, `->`, `->>`), five parameter forms (`?`, `?NNN`, `:name`, `@name`, `$name`), and `--`/`/* */` comments. Every token carries a line/column/byte-offset `Span`; malformed input always yields a `TokenKind::Error`, never a panic.
- `tests/tokenizer_proptest.rs`: tokenize/print roundtrip and never-panics-on-arbitrary-input property tests.

### Changed

- `.openspec/specs/002-parser/spec.md`: Requirement 1 flips `(planned)` → active, all 4 scenarios test-linked.

## [0.4.0] - 2026-08-14 — V1 phase 4: the deliverable

`sqlite-rs dump`/`export` CLI — V1 step 9, epic #5's acceptance-gate ticket (#37, #49).

### Added

- **`sqlite-rs dump <file>` / `sqlite-rs export <file>`** (`src/bin/sqlite-rs.rs`): schema + all rows of every readable table (rowid and WITHOUT ROWID), with rowid-alias substitution for `INTEGER PRIMARY KEY` columns and REAL-affinity 0/1 constant-optimization handling. Virtual tables and any table that fails to decode are skipped with a warning on stderr rather than aborting the whole dump; `export`'s per-table output filenames are sanitized against the source database's (untrusted) table names to prevent path traversal. Both subcommands return a non-`SUCCESS` exit code when any table was skipped or failed to write, so scripted callers can detect partial output.
- **`src/format.rs`**: `-list`/`-csv` value rendering verified byte-identical to a real, read-only `sqlite3` process — REAL formatting (`%.15g`-equivalent), blob-as-`X'HEX'`, and `sqlite3`'s actual (non-RFC4180) CSV quoting heuristic.
- `tests/corpus/dump_oracle_test.rs`: shells out to a real `sqlite3 -readonly` and diffs `dump_database`'s rendering against it across every table of every corpus fixture (list and csv mode); `tests/corpus/harness.rs`'s previously-stubbed fixture reader now does a real open-and-dump.

### Changed

- `TableSchema` gains a `sql` field (the verbatim `CREATE TABLE` text), needed to reproduce schema DDL and column type/affinity info.
- `Makefile`'s `mvl-limit` qualified-subset gate excludes `src/bin/*` — a CLI's stdout/stderr is an I/O boundary, the same way `src/vfs` already is the designated `unsafe`/`dyn` boundary.

### V1 exit gate

- Dump/export oracle parity across all corpus fixture families: done (this release)
- Mutation-testing run, assurance-dashboard check, epic #5 close: tracked separately (#37 item (e)) — completing them finishes V1 without a further version bump

## [0.3.0] - 2026-08-14 — V1 phase 3: mid-life databases

Pager read path, WAL frame reading, and safe-reader locking — epic #5 steps 2 and 6 (#35, #36), the `journalstates` fixture family (#21), and the safe-reader concurrency scope validated by spike 005 (#8) and implemented via #50/#45. Phase 3 = reading databases *mid-life*, not just at rest.

### Added

- **`Pager`** (`src/pager/mod.rs`, #35): a `PageSource` implementation sitting between the VFS and the b-tree cursor. Refuses to open a database with a hot rollback journal (valid magic header) rather than risk serving pre-rollback pages as committed data; otherwise wraps `VfsPageSource` unchanged, so `TableCursor<Pager>`/`IndexCursor<Pager>` are byte-identical to the `VfsPageSource`-based cursors on every at-rest fixture, including auto-vacuum databases.
- **WAL frame reading** (`src/pager/wal.rs`, #36): WAL header parsing (both checksum-endianness variants — magic `0x377f0682` is native byte order, the common case), frame walk with checksum/salt validation, and a committed-page index merged transparently into `Pager`. Read-only, quiescent-file recovery — no `-shm` file required for the recovery path.
- **Safe-reader locking** (#50, #45; byte offsets and sequences validated against a live stock `sqlite3` by spike 005, not re-derived):
  - `Pager::open` acquires the journal-mode SHARED byte-range `fcntl` lock (`PENDING_BYTE+2` / `SHARED_SIZE`) before serving any page, released on drop (`src/vfs/lock.rs`, opaque `FileLock` type — no `dyn`/`unsafe` leaks outside `src/vfs/`).
  - **Busy detection** (`VfsError::Locked`): lock contention (`EAGAIN`/`EACCES`) surfaces as a distinguishable "database is locked" error, not a generic I/O failure.
  - **WAL `-shm` reader-mark protocol** (`src/vfs/shm.rs`): on WAL-mode databases, `Pager::open` claims a `WAL_READ_LOCK` slot and publishes its `aReadMark` at the WAL's current `mxFrame` (read only *after* the exclusive slot claim), so a live `sqlite3` checkpointer backs off instead of truncating frames the reader depends on. Released on drop.
  - Cross-process (`fork`-based) tests for the locking paths — POSIX record locks never conflict within one process.
- **`journalstates` fixture family** (#21): hot-journal and four WAL-pending fixture variants (primary, trailing/spilled, stale/foreign-salt, big-endian checksum), reusing spike #7's fixture-generation tricks.
- **Spec 007-pager**: hot-journal detection, page-view zero-behavior-change, WAL frame reading; new 001-architecture Req-4 scenario "Reader takes a SHARED lock before serving pages".
- `Vfs::companion_path`, closing spec 003 Req-1's previously-unimplemented "Companion file detection" scenario.
- Second fuzz target (`fuzz/fuzz_targets/wal_frames.rs`, `make fuzz-wal`) for the "malformed WAL never panics" acceptance criterion.

### Changed

- `src/lib.rs`: `#![forbid(unsafe_code)]` → `#![deny(unsafe_code)]` — `forbid` cannot be locally overridden and `src/vfs/lock.rs` needs a scoped `#![allow(unsafe_code)]` for raw `fcntl`. The `unsafe` boundary stays exactly where the plan designated it: `src/vfs/`.
- `libc` added as a direct dependency (previously only transitive).

### Fixed

- `tests/corpus/regen_test.rs`'s reproducibility check assumed byte-identical regeneration corpus-wide — spec 004 Req-2 already allowed for "byte-identity not required where sqlite3 embeds nondeterminism," but nothing had exercised that carve-out until `journalstates`'s WAL salts/journal nonces became the corpus's first nondeterministic fixture family. Now compared by size for that family only.

### Deferred

- **Per-inode fd-cache** for the POSIX `close()`-drops-all-locks trap (#45): deliberately not built — nothing in the crate opens two fds to the same path (main db, `-wal`, `-shm` are three distinct paths, each opened once), so there is no bug for it to fix yet. Revisit when a write path or live-refresh read path needs a second fd to an already-locked file.
- **Linux exercise of the locking interop**: owned by #42; CI already runs the full suite (including lock/shm tests) on `ubuntu-latest`.

### Known limitations

- Mapping a `-shm` file that another process may truncate can raise `SIGBUS` (an uncatchable process termination, not a Rust panic) if the mapping outlives the file's backing pages. Inherent to the mmap approach without a `SIGBUS` handler; the threat model here is a cooperating local `sqlite3` writer, not an adversarial one — documented in `src/vfs/shm.rs` rather than mitigated.

## [0.2.0] - 2026-08-14 — V1 phase 2: b-trees and schema

Read-only table and index b-tree cursors, plus the minimal DDL reader — epic #5 steps 4, 5, 7 (#32, #33, #34).

### Added

- **Table b-tree cursor** (`src/btree/`, #32): `TableCursor` (`first()`/`next()`/`seek(rowid)`) over table b-trees (page types 0x05/0x0d), overflow-chain reassembly, page-1 cell-pointer-array trap; `src/vfs/page_source.rs` generic `PageSource` trait + `VfsPageSource` adapter
- **Index b-tree cursor** (`src/btree/index.rs`, #33): `IndexCursor` (`first()`/`next()`/`seek(target)`) over index b-trees (page types 0x02/0x0a), minimal key comparison (NULL < numeric < text < blob, BINARY collation); makes WITHOUT ROWID tables readable
- **Minimal DDL reader** (`src/schema/ddl_reader.rs`, #34): `read_schema()` decodes `sqlite_master` into `TableSchema` (name, root_page, columns, without_rowid, strict, is_virtual) with zero dependency on a future full parser; unparseable/virtual-table DDL degrades to raw-row access, never an error
- **Spec 006-btree**: page/cell/overflow byte format, transcribed from SQLite's file format and validated against a real oracle
- First fuzz target in the repo (`fuzz/fuzz_targets/btree_cursor.rs`, `cargo-fuzz`, `make fuzz-btree`)

### Fixed

- `TableCursor::seek` no longer accumulates against the `first`/`next` traversal's page-visited budget, so a long-lived cursor doing many point lookups can't spuriously fail
- Overflow-chain reassembly now detects a chain that revisits a page (cycle) instead of relying solely on a flat hop cap, closing a resource-exhaustion path where a small malicious file could force very large reads/allocations

## [0.1.0] - 2026-08-14 — V1 phase 1: format core

First milestone: the pure-computation core of the Tier 0 READ CORE, plus the assurance machinery. Epic #5 steps 1, 3, 8.

### Added

- **Record format decoder** (`src/record/`, #9): varints (1-9 bytes), all serial types (NULL, all integer widths, f64 bit-exact, constants, BLOB, TEXT), all three text encodings (UTF-8/16LE/16BE), structured errors — no panics on malformed input
- **Database header parser** (`src/header.rs`, #11): full 100-byte header, page sizes 512-65536 (incl. `1` = 65536), reserved bytes, WAL-mode detection, text encoding
- **Read-only VFS** (`src/vfs/`, #11): `Vfs`/`VfsFile` traits, Unix + in-memory implementations passing a shared contract suite
- **Fixture corpus + pinned oracle harness** (`tests/corpus/`, #10): reproducible generation (`tools/gen_fixtures.sh`), oracle version pinning, diff harness green-with-skips from day one
- **Assurance tooling**: `make assurance` dashboard (spec↔code↔test traceability, per-scenario links, symbol validation, dead-link detection), `make mvl-limit` qualified-subset gate (#23), coverage gate CI (#16, #24)
- **Specs**: 001-architecture (tier model), 002-parser, 003-file-format, 004-corpus; 12-block value plan with drop order and concurrency contract
- **Spikes**: 001 (parser toolchains), 002 (end-to-end file read — GO, findings in `tests/spike/002_file_reading/findings.md`)

### Assurance at this release

- `#![forbid(unsafe_code)]` — whole crate
- mvl-limit: all files in the qualified subset
- Traceability: 10/10 requirements implemented (specs 003/004), 22/30 scenarios test-backed, 0 dead links
