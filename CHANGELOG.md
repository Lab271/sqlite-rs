# Changelog

All notable changes to sqlite-rs. Format follows [Keep a Changelog](https://keepachangelog.com/), versioning follows [SemVer](https://semver.org/). Pre-1.0: minor bumps may break the public API.

**Versioning policy:** one minor version per completed plan phase — the version number tells the plan's story, sub-steps stay inside a phase. V1 (READ CORE) = 0.1.0 through 0.4.0. *(History note: internal iterations briefly numbered 0.4.0–0.6.0 were renumbered into the phase scheme on 14 Aug 2026, before any tag or publication of those versions existed.)*

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
