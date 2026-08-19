# Changelog

All notable changes to sqlite-rs. Format follows [Keep a Changelog](https://keepachangelog.com/), versioning follows [SemVer](https://semver.org/). Pre-1.0: minor bumps may break the public API.

**Versioning policy:** one minor version per completed plan phase — the version number tells the plan's story, sub-steps stay inside a phase. V1 (READ CORE) = 0.1.0 through 0.4.0. *(History note: internal iterations briefly numbered 0.4.0–0.6.0 were renumbered into the phase scheme on 14 Aug 2026, before any tag or publication of those versions existed.)*

## [Unreleased]

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
