---
domain: storage
version: 0.1.0
status: draft
date: 2026-08-14
---

# 006 — B-Tree

The read-only table and index b-tree cursors: page header/cell-pointer-array layout, cell decode, key comparison, and overflow-chain reassembly. Backs V1 step 4 (#32) and step 5 (#33), part of epic #5 phase 2. Depends on spec 003 (header, VFS, record decode). Validated in part by spike 005 (#12), which exercised interior-node traversal and overflow reassembly against real multi-page/overflow fixtures before this spec was written.

Everything in this spec is **Tier 0 READ CORE** — never droppable.

## Philosophy

Transcribed from [fileformat2.html](https://www.sqlite.org/fileformat2.html)'s b-tree page and cell-payload-overflow sections, verified byte-by-byte against the pinned oracle (spec 004) rather than against any secondary source. `src/btree/` is not exempt from the `mvl-limit` qualified-subset gate (no `unsafe`/`dyn`/explicit lifetimes); the `PageSource` trait this module depends on is defined in `src/vfs/` for exactly that reason. Tier 0 core carries no exclusions at all — where the gate and a convenience collide here, the code changes: `TableCursor::prev()`'s precondition is a real `BtreeError::CursorNotPositioned` rather than a `debug_assert!` (outside the gate's macro allowlist), which also makes the check survive into release builds. The exempt set is `src/vfs/` plus, since #90, the VDBE's `Rc<dyn PageSource>` handle in `src/vdbe/{exec,cursor}.rs` — see the Makefile's boundary policy and #114.

## Requirements

### Requirement 1: Table B-Tree Page Format [MUST]

The system MUST parse table b-tree pages (page type `0x0d` leaf, `0x05` interior): the 8-byte leaf / 12-byte interior page header, the cell-pointer array, and — for page 1 specifically — resolve cell-pointer-array offsets relative to byte 0 of the page, not to the 100-byte file header that precedes the b-tree page header on that page only.

**Implementation:** `src/btree.rs`

**Tests:** inline `#[cfg(test)]` in `src/btree.rs`

**Corpus:** `tests/corpus/fixtures/btrees/`

#### Scenario: Page-1 trap

- GIVEN page 1, which carries the 100-byte file header before its own b-tree page header
- WHEN the cursor reads page 1 as a table b-tree root (e.g. `sqlite_master`, always rootpage 1)
- THEN cell-pointer-array offsets MUST resolve relative to byte 0 of the page, not to byte 100

**Tests:** `src/btree.rs::page_one_trap_sqlite_master_root_is_page_one`

#### Scenario: Interior-node depth-first traversal

- GIVEN a table b-tree spanning multiple pages (interior + leaf nodes)
- WHEN the cursor walks the full table via `first()`/`next()`
- THEN every row is visited exactly once, in ascending rowid order, matching the pinned oracle row-for-row

**Tests:** `src/btree.rs::table_multipage_full_scan_matches_oracle`

#### Scenario: Point lookup by rowid

- GIVEN a multi-page table b-tree
- WHEN `seek(rowid)` is called for an existing rowid, the first rowid, the last rowid, or a rowid that does not exist
- THEN the result MUST match the pinned oracle's point-lookup result (`Some` row or `None`)

**Tests:** `src/btree.rs::table_multipage_seek_matches_oracle`

### Requirement 2: Cell Payload Overflow [MUST]

The system MUST compute a cell's local payload size using SQLite's overflow formula (`max_local = usable_size - 35`; `min_local = ((usable_size - 12) * 32 / 255) - 23`; `K = min_local + (payload_len - min_local) % (usable_size - 4)`) and, when the payload overflows, walk the overflow-page chain (each overflow page's first 4 bytes are the next page number, or 0 to end the chain) to reassemble the full payload byte-for-byte.

**Implementation:** `src/btree.rs`

**Tests:** inline `#[cfg(test)]` in `src/btree.rs`

**Corpus:** `tests/corpus/fixtures/btrees/` (`overflow_single_page.db`, `overflow_multi_page.db`)

#### Scenario: Single-page overflow

- GIVEN a cell whose payload spills into exactly one overflow page
- WHEN the cursor reads that row
- THEN the reassembled payload MUST be byte-identical to the pinned oracle's value

**Tests:** `src/btree.rs::overflow_single_page_payload_is_byte_identical_to_oracle`

#### Scenario: Multi-page overflow chain

- GIVEN a cell whose payload spans a chain of overflow pages (validated at 14 pages in spike 005, #12)
- WHEN the cursor reads that row
- THEN the reassembled payload MUST be byte-identical to the pinned oracle's value

**Tests:** `src/btree.rs::overflow_multi_page_payload_is_byte_identical_to_oracle`

### Requirement 3: Malformed Input Never Panics [MUST]

Every page-parsing and overflow-reassembly path MUST return `Err`, never panic, on truncated pages, unexpected page types, out-of-bounds cell pointers, implausible payload-length claims, and overflow chains that end early or exceed a sanity-bounded page count (cycle protection). A fuzz target MUST exercise this against arbitrary bytes.

**Implementation:** `src/btree.rs`

**Tests:** inline `#[cfg(test)]` in `src/btree.rs`; `tests/fuzz/fuzz_targets/btree_cursor.rs`

#### Scenario: Truncated or malformed page

- GIVEN a page shorter than its declared header, or with an unrecognized page-type byte
- WHEN the cursor attempts to read it
- THEN the cursor MUST return `Err`, never panic

**Tests:** `src/btree.rs::truncated_page_errors_not_panics`, `src/btree.rs::unexpected_page_type_errors_not_panics`, `src/btree/index.rs::truncated_page_errors_not_panics`, `src/btree/index.rs::unexpected_page_type_errors_not_panics`

#### Scenario: Broken overflow chain

- GIVEN an overflow chain that reaches a terminating page (next pointer `0`) before all declared payload bytes have been read
- WHEN the cursor reassembles the payload
- THEN the cursor MUST return `Err`, never panic or silently truncate

**Tests:** `src/btree.rs::overflow_chain_hitting_page_zero_early_errors_not_panics`

#### Scenario: Fuzz safety

- GIVEN arbitrary byte input treated as a page
- WHEN the fuzz target runs
- THEN the cursor MUST never panic, regardless of input

**Tests:** `tests/fuzz/fuzz_targets/btree_cursor.rs`

### Requirement 4: Rowid-Alias Columns Are Not This Layer's Job [SHOULD]

A column declared exactly `INTEGER PRIMARY KEY` is not stored in the record (SQLite encodes it as `NULL` and expects the reader to substitute the cell's own rowid). This module MUST return the record payload faithfully, including that `NULL`, rather than attempting schema-aware substitution — that requires knowing which column, if any, is the alias, which is DDL/schema information this layer does not have. Found and documented in spike 005 (#12); the substitution itself is deferred to the DDL reader (#34) or a higher row-assembly layer.

**Implementation:** `src/btree.rs` (module doc's rowid-alias note)

#### Scenario: Rowid-alias column decodes as NULL, not silently wrong

- GIVEN a table with an `INTEGER PRIMARY KEY` column
- WHEN the cursor decodes a row's payload via `record::decode_record`
- THEN the alias column's decoded value MUST be `Value::Null` (faithful to the stored bytes), and callers needing the real value MUST substitute the row's own `rowid`

**Tests:** `src/schema/ddl_reader.rs` (planned)

Covered functionally once #34 (DDL reader) lands and can identify the alias column; flagging here rather than leaving this scenario unlinked.

### Requirement 5: Index B-Tree Page Format [MUST]

The system MUST parse index b-tree pages (page type `0x0a` leaf, `0x02` interior). Unlike table b-tree interior cells (routing-only, no payload), index b-tree interior cells carry a full key payload — a real, sorted entry with a left-child subtree of lesser keys, not merely a separator — so in-order traversal MUST yield each interior cell's own key interleaved with descending into its children. WITHOUT ROWID tables are stored as index b-trees (confirmed on a real fixture in spike 005, #12: FTS5's `t_idx`/`t_config` shadow tables) and MUST be readable through this cursor.

**Implementation:** `src/btree/index.rs`

**Tests:** inline `#[cfg(test)]` in `src/btree/index.rs`

**Corpus:** `tests/corpus/fixtures/btrees/` (`index.db`, `without_rowid.db`)

#### Scenario: Secondary-index walk in BINARY order

- GIVEN a multi-page secondary index over a rowid table
- WHEN the cursor walks the full index via `first()`/`next()`
- THEN every entry is visited exactly once, in ascending BINARY-collation key order, matching the pinned oracle key-for-key (including lexicographic, not numeric, text ordering)

**Tests:** `src/btree/index.rs::secondary_index_walk_matches_oracle_binary_order`

#### Scenario: WITHOUT ROWID table read via index b-tree

- GIVEN a `WITHOUT ROWID` table (stored as an index b-tree keyed on its declared primary key)
- WHEN the cursor walks the full table via `first()`/`next()`
- THEN every row is visited exactly once, in ascending primary-key order, matching the pinned oracle row-for-row (this is spec 001 Req 4's "WITHOUT ROWID" scenario, made concrete here)

**Tests:** `src/btree/index.rs::without_rowid_table_is_readable_as_index_btree`

#### Scenario: Malformed index page

- GIVEN a page shorter than its declared header, or with an unrecognized page-type byte, where an index b-tree page was expected
- WHEN the cursor attempts to read it
- THEN the cursor MUST return `Err`, never panic

**Tests:** `src/btree/index.rs::truncated_page_errors_not_panics`, `src/btree/index.rs::unexpected_page_type_errors_not_panics`

### Requirement 6: Minimal Key Comparison and Seek [SHOULD]

The system SHOULD provide enough key ordering to walk an index b-tree correctly (NULL < numeric < text < blob, BINARY collation only — no other collating sequences at Tier 0) and a minimal `seek` that finds the first entry not less than a target key. `seek` MAY be a linear scan rather than a tree descent — Tier 0 needs enough ordering to walk, not a fully general logarithmic seek — trading efficiency for a simpler, harder-to-get-wrong implementation.

**Implementation:** `src/btree/index.rs`

**Tests:** inline `#[cfg(test)]` in `src/btree/index.rs`

#### Scenario: Point lookup by key

- GIVEN a multi-page secondary index
- WHEN `seek(target)` is called for an existing key
- THEN the returned entry's key MUST match the pinned oracle's point-lookup result

**Tests:** `src/btree/index.rs::secondary_index_seek_matches_oracle`

### Requirement 7: No Fixture Yet Covers an Overflowing Index Key [SHOULD]

The originating issue's corpus section expects an index fixture with an overflowing key; `tests/corpus/fixtures/btrees/index.db`'s actual generated content (`b TEXT`, max length 15 bytes) does not exercise this — overflow reassembly on index cells reuses the same `reassemble_payload` function already proven byte-identical against table-cell overflow (Requirement 2), so the residual risk is low, but this is a real coverage gap, not a silent oversight.

**Implementation:** `tools/gen_fixtures.sh` (planned — no fixture with an overflowing index key exists yet)

#### Scenario: Overflowing index key

- GIVEN an index whose key column is large enough to overflow into one or more overflow pages
- WHEN the cursor reads that entry
- THEN the reassembled key payload MUST be byte-identical to the pinned oracle's value

**Tests:** `tools/gen_fixtures.sh` (planned)
