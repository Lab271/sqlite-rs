# Changelog

All notable changes to sqlite-rs. Format follows [Keep a Changelog](https://keepachangelog.com/), versioning follows [SemVer](https://semver.org/). Pre-1.0: minor bumps may break the public API.

## [0.2.0] - 2026-08-14

Read-only table and index b-tree cursors, plus the minimal DDL reader — V1 phase 2, epic #5 steps 4-6 (#32, #33, #34).

### Added

- **Table b-tree cursor** (`src/btree/`, #32): `TableCursor` (`first()`/`next()`/`seek(rowid)`) over table b-trees (page types 0x05/0x0d), overflow-chain reassembly, page-1 cell-pointer-array trap; `src/vfs/page_source.rs` generic `PageSource` trait + `VfsPageSource` adapter
- **Index b-tree cursor** (`src/btree/index.rs`, #33): `IndexCursor` (`first()`/`next()`/`seek(target)`) over index b-trees (page types 0x02/0x0a), minimal key comparison (NULL < numeric < text < blob, BINARY collation); makes WITHOUT ROWID tables readable
- **Minimal DDL reader** (`src/schema/ddl_reader.rs`, #34): `read_schema()` decodes `sqlite_master` into `TableSchema` (name, root_page, columns, without_rowid, strict, is_virtual) with zero dependency on a future full parser; unparseable/virtual-table DDL degrades to raw-row access, never an error
- **Spec 006-btree**: page/cell/overflow byte format, transcribed from SQLite's file format and validated against a real oracle
- First fuzz target in the repo (`fuzz/fuzz_targets/btree_cursor.rs`, `cargo-fuzz`, `make fuzz-btree`)

### Fixed

- `TableCursor::seek` no longer accumulates against the `first`/`next` traversal's page-visited budget, so a long-lived cursor doing many point lookups can't spuriously fail
- Overflow-chain reassembly now detects a chain that revisits a page (cycle) instead of relying solely on a flat hop cap, closing a resource-exhaustion path where a small malicious file could force very large reads/allocations

## [0.1.0] - 2026-08-14

First milestone: the pure-computation core of the Tier 0 READ CORE, plus the assurance machinery. V1 phase 1 — epic #5 steps 1, 3, 8.

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
