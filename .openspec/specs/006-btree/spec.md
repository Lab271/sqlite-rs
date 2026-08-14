---
domain: storage
version: 0.1.0
status: draft
date: 2026-08-14
---

# 006 — B-Tree

The read-only table b-tree cursor: page header/cell-pointer-array layout, cell decode, and overflow-chain reassembly. Backs V1 step 4 (#32), part of epic #5 phase 2. Depends on spec 003 (header, VFS, record decode). Validated in part by spike 005 (#12), which exercised interior-node traversal and overflow reassembly against real multi-page/overflow fixtures before this spec was written.

Everything in this spec is **Tier 0 READ CORE** — never droppable.

## Philosophy

Transcribed from [fileformat2.html](https://www.sqlite.org/fileformat2.html)'s b-tree page and cell-payload-overflow sections, verified byte-by-byte against the pinned oracle (spec 004) rather than against any secondary source. `src/btree/` is not exempt from the `mvl-limit` qualified-subset gate (no `unsafe`/`dyn`/explicit lifetimes) — only `src/vfs/` is; the `PageSource` trait this module depends on is defined in `src/vfs/` for exactly that reason.

## Requirements

### Requirement 1: Table B-Tree Page Format [MUST]

The system MUST parse table b-tree pages (page type `0x0d` leaf, `0x05` interior): the 8-byte leaf / 12-byte interior page header, the cell-pointer array, and — for page 1 specifically — resolve cell-pointer-array offsets relative to byte 0 of the page, not to the 100-byte file header that precedes the b-tree page header on that page only.

**Implementation:** `src/btree/mod.rs`

**Tests:** inline `#[cfg(test)]` in `src/btree/mod.rs`

**Corpus:** `tests/corpus/fixtures/btrees/`

#### Scenario: Page-1 trap

- GIVEN page 1, which carries the 100-byte file header before its own b-tree page header
- WHEN the cursor reads page 1 as a table b-tree root (e.g. `sqlite_master`, always rootpage 1)
- THEN cell-pointer-array offsets MUST resolve relative to byte 0 of the page, not to byte 100

**Tests:** `src/btree/mod.rs::page_one_trap_sqlite_master_root_is_page_one`

#### Scenario: Interior-node depth-first traversal

- GIVEN a table b-tree spanning multiple pages (interior + leaf nodes)
- WHEN the cursor walks the full table via `first()`/`next()`
- THEN every row is visited exactly once, in ascending rowid order, matching the pinned oracle row-for-row

**Tests:** `src/btree/mod.rs::table_multipage_full_scan_matches_oracle`

#### Scenario: Point lookup by rowid

- GIVEN a multi-page table b-tree
- WHEN `seek(rowid)` is called for an existing rowid, the first rowid, the last rowid, or a rowid that does not exist
- THEN the result MUST match the pinned oracle's point-lookup result (`Some` row or `None`)

**Tests:** `src/btree/mod.rs::table_multipage_seek_matches_oracle`

### Requirement 2: Cell Payload Overflow [MUST]

The system MUST compute a cell's local payload size using SQLite's overflow formula (`max_local = usable_size - 35`; `min_local = ((usable_size - 12) * 32 / 255) - 23`; `K = min_local + (payload_len - min_local) % (usable_size - 4)`) and, when the payload overflows, walk the overflow-page chain (each overflow page's first 4 bytes are the next page number, or 0 to end the chain) to reassemble the full payload byte-for-byte.

**Implementation:** `src/btree/mod.rs`

**Tests:** inline `#[cfg(test)]` in `src/btree/mod.rs`

**Corpus:** `tests/corpus/fixtures/btrees/` (`overflow_single_page.db`, `overflow_multi_page.db`)

#### Scenario: Single-page overflow

- GIVEN a cell whose payload spills into exactly one overflow page
- WHEN the cursor reads that row
- THEN the reassembled payload MUST be byte-identical to the pinned oracle's value

**Tests:** `src/btree/mod.rs::overflow_single_page_payload_is_byte_identical_to_oracle`

#### Scenario: Multi-page overflow chain

- GIVEN a cell whose payload spans a chain of overflow pages (validated at 14 pages in spike 005, #12)
- WHEN the cursor reads that row
- THEN the reassembled payload MUST be byte-identical to the pinned oracle's value

**Tests:** `src/btree/mod.rs::overflow_multi_page_payload_is_byte_identical_to_oracle`

### Requirement 3: Malformed Input Never Panics [MUST]

Every page-parsing and overflow-reassembly path MUST return `Err`, never panic, on truncated pages, unexpected page types, out-of-bounds cell pointers, implausible payload-length claims, and overflow chains that end early or exceed a sanity-bounded page count (cycle protection). A fuzz target MUST exercise this against arbitrary bytes.

**Implementation:** `src/btree/mod.rs`

**Tests:** inline `#[cfg(test)]` in `src/btree/mod.rs`; `fuzz/fuzz_targets/btree_cursor.rs`

#### Scenario: Truncated or malformed page

- GIVEN a page shorter than its declared header, or with an unrecognized page-type byte
- WHEN the cursor attempts to read it
- THEN the cursor MUST return `Err`, never panic

**Tests:** `src/btree/mod.rs::truncated_page_errors_not_panics`, `src/btree/mod.rs::unexpected_page_type_errors_not_panics`

#### Scenario: Broken overflow chain

- GIVEN an overflow chain that reaches a terminating page (next pointer `0`) before all declared payload bytes have been read
- WHEN the cursor reassembles the payload
- THEN the cursor MUST return `Err`, never panic or silently truncate

**Tests:** `src/btree/mod.rs::overflow_chain_hitting_page_zero_early_errors_not_panics`

#### Scenario: Fuzz safety

- GIVEN arbitrary byte input treated as a page
- WHEN the fuzz target runs
- THEN the cursor MUST never panic, regardless of input

**Tests:** `fuzz/fuzz_targets/btree_cursor.rs`

### Requirement 4: Rowid-Alias Columns Are Not This Layer's Job [SHOULD]

A column declared exactly `INTEGER PRIMARY KEY` is not stored in the record (SQLite encodes it as `NULL` and expects the reader to substitute the cell's own rowid). This module MUST return the record payload faithfully, including that `NULL`, rather than attempting schema-aware substitution — that requires knowing which column, if any, is the alias, which is DDL/schema information this layer does not have. Found and documented in spike 005 (#12); the substitution itself is deferred to the DDL reader (#34) or a higher row-assembly layer.

**Implementation:** `src/btree/mod.rs` (module doc's rowid-alias note)

#### Scenario: Rowid-alias column decodes as NULL, not silently wrong

- GIVEN a table with an `INTEGER PRIMARY KEY` column
- WHEN the cursor decodes a row's payload via `record::decode_record`
- THEN the alias column's decoded value MUST be `Value::Null` (faithful to the stored bytes), and callers needing the real value MUST substitute the row's own `rowid`

**Tests:** `src/schema/ddl_reader.rs` (planned)

Covered functionally once #34 (DDL reader) lands and can identify the alias column; flagging here rather than leaving this scenario unlinked.
