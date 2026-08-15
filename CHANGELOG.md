# Changelog

All notable changes to sqlite-rs. Format follows [Keep a Changelog](https://keepachangelog.com/), versioning follows [SemVer](https://semver.org/). Pre-1.0: minor bumps may break the public API.

**Versioning policy:** one minor version per completed plan phase — the version number tells the plan's story, sub-steps stay inside a phase. V1 (READ CORE) = 0.1.0 through 0.4.0. *(History note: internal iterations briefly numbered 0.4.0–0.6.0 were renumbered into the phase scheme on 14 Aug 2026, before any tag or publication of those versions existed.)*

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
