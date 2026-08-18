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

Transcribed from [fileformat2.html](https://www.sqlite.org/fileformat2.html)'s b-tree page and cell-payload-overflow sections, verified byte-by-byte against the pinned oracle (spec 004) rather than against any secondary source. `src/btree/` is not exempt from the `mvl-limit` qualified-subset gate (no `unsafe`/`dyn`/explicit lifetimes); the `PageSource` trait this module depends on is defined in `src/vfs/` for exactly that reason. Tier 0 core carries no exclusions at all — where the gate and a convenience collide here, the code changes: `TableCursor::prev()`'s precondition is a real `BtreeError::CursorNotPositioned` rather than a `debug_assert!` (outside the gate's macro allowlist), which also makes the check survive into release builds. The exempt set is `src/vfs/` plus, since #90, the VDBE's `Rc<dyn PageSource>` handle in `src/vdbe/{exec,cursor}.rs` — a deliberate, permanent second boundary (ADR-0013 considered genericizing `Vm` over `P: PageSource` instead, #114, and rejected it; see the Makefile's boundary policy).

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

### Requirement 8: Leaf Cell Insert Without Split [MUST]

The system MUST insert a `(rowid, payload)` row into a table b-tree leaf page that has room for it: encoding the cell (payload-length varint + rowid varint + local payload bytes, plus a 4-byte overflow-page pointer per Requirement 2 when the payload doesn't fit locally), locating the rowid-ordered insertion position, and rewriting the page's cell-pointer array and cell count so the leaf's cells remain in strict ascending rowid order. Inserting a rowid that already exists in the leaf MUST return `Err`, never silently overwrite or duplicate.

**Implementation:** `src/btree/insert.rs::insert_into_leaf`, `src/btree/insert.rs::encode_leaf_cell`

#### Scenario: Single-row insert into an otherwise-empty leaf

- GIVEN a table with no existing rows
- WHEN one row is inserted via `insert_row`
- THEN stock `sqlite3` MUST open the file, pass `PRAGMA integrity_check`, and read back the exact row

**Tests:** `tests/corpus/btree_insert_test.rs::insert_single_row_no_split`

#### Scenario: Duplicate rowid is rejected

- GIVEN a leaf that already holds a row with rowid `R`
- WHEN a second insert targets rowid `R`
- THEN `insert_row` MUST return `Err(BtreeError::DuplicateRowid)`, leaving the page unchanged

**Tests:** inline `#[cfg(test)]` in `src/btree/insert.rs` (planned — not yet written; duplicate-rowid rejection is implemented but not corpus/oracle-tested)

### Requirement 9: Leaf Split with Median Propagation [MUST]

When a cell won't fit in its target leaf, the system MUST allocate a new page (via the pager's freelist-aware allocator), distribute the leaf's existing cells plus the new cell roughly in half by count (the original page keeps the lower rowids, the newly allocated page takes the upper half), and propagate the split to the parent interior page by inserting a routing cell (child = original leaf, key = the original leaf's new maximum rowid) immediately before whatever routing entry previously pointed at the original leaf, redirecting that entry to the new page.

**Implementation:** `src/btree/insert.rs::insert_into_leaf`, `src/btree/insert.rs::insert_into_parent`

#### Scenario: Insert forces exactly one leaf split

- GIVEN enough rows inserted that a single leaf page's free space is exhausted, but not enough to grow past one interior level
- WHEN the split runs
- THEN stock `sqlite3` MUST open the file, pass `PRAGMA integrity_check`, and read back every row, in order, unchanged

**Tests:** `tests/corpus/btree_insert_test.rs::insert_forces_a_leaf_split`

### Requirement 10: Cascading Interior Splits [MUST]

When an interior page's routing-cell insert (propagated up from a child split, per Requirement 9) doesn't fit, the system MUST split the interior page itself: promoting the median key to the grandparent without duplicating it in either child, with the left interior page's rightmost pointer becoming the promoted entry's former child pointer. This MUST recurse arbitrarily many levels up the ancestor chain produced by the leaf-to-root path search.

**Implementation:** `src/btree/insert.rs::insert_into_parent`

#### Scenario: Insert forces multiple cascading interior splits

- GIVEN enough rows inserted to overflow more than one interior page's worth of routing cells
- WHEN the cascade runs
- THEN stock `sqlite3` MUST open the file, pass `PRAGMA integrity_check`, and read back every row, in order, unchanged

**Tests:** `tests/corpus/btree_insert_test.rs::insert_forces_cascading_splits_and_a_root_split`

### Requirement 11: Root Split [MUST]

Because a table's root page number is fixed (referenced by its `sqlite_master` entry) and can never be relocated, the system MUST handle a split that reaches the root by relocating the root's current content (leaf or interior, verbatim) to a newly allocated page, then reinitializing the root page in place as a fresh interior page holding a single routing cell (child = the relocated page, key = the promoted divider) and the split's new sibling as the rightmost pointer. This MUST work whether the root is page 1 (the 100-byte file-header offset per the page-1 trap) or any other page number.

**Implementation:** `src/btree/insert.rs::root_split`

#### Scenario: Insert forces a root split

- GIVEN enough rows inserted that even the root page overflows
- WHEN the root split runs
- THEN stock `sqlite3` MUST open the file, pass `PRAGMA integrity_check`, and read back every row, including the first and last rowids inserted, unchanged

**Tests:** `tests/corpus/btree_insert_test.rs::insert_forces_cascading_splits_and_a_root_split`

#### Scenario: Root split on page 1 preserves the 100-byte file header

- GIVEN a table b-tree rooted at page 1 (`sqlite_master` itself), the one root that physically shares its page with the 100-byte file header
- WHEN enough rows are inserted that page 1 itself splits into an interior root
- THEN stock `sqlite3` MUST still open the file, pass `PRAGMA integrity_check`, and every row present before the split (including the file header's own fields, proven by the file still opening at all) MUST survive unchanged

**Tests:** `tests/corpus/btree_insert_test.rs::insert_into_page_one_root_preserves_the_file_header_across_a_split`

#### Scenario: Bulk insert stays oracle-identical at scale

- GIVEN 1000 rows inserted one at a time (spanning multiple leaf and interior splits)
- WHEN the file is reopened by stock `sqlite3`
- THEN `PRAGMA integrity_check` MUST pass and every row MUST read back identically, in rowid order

**Tests:** `tests/corpus/btree_insert_test.rs::bulk_insert_1000_rows_is_oracle_identical`

#### Scenario: Overflow payload combined with a split

- GIVEN rows whose payload is large enough to require an overflow chain (Requirement 2), inserted in enough quantity to also force a leaf split
- WHEN the file is reopened by stock `sqlite3`
- THEN `PRAGMA integrity_check` MUST pass and every row's overflowing payload MUST read back byte-identical

**Tests:** `tests/corpus/btree_insert_test.rs::insert_with_overflow_payload_combined_with_a_split`

### Requirement 12: Leaf Cell Delete [MUST]

The system MUST delete the row with a given rowid from a table b-tree leaf page: locating the cell by rowid, removing it from the cell-pointer array, and rewriting the page's remaining cells (in order) and cell count. Deleting a rowid that doesn't exist in the tree MUST return `Err`, leaving the tree unchanged.

**Implementation:** `src/btree/delete.rs::delete_row`

#### Scenario: Deleting a missing rowid errors without mutating the tree

- GIVEN a table b-tree that does not contain rowid `R`
- WHEN `delete_row` is called with rowid `R`
- THEN it MUST return `Err(BtreeError::RowidNotFound)`, leaving the page unchanged

**Tests:** `src/btree/delete.rs::tests::deleting_a_missing_rowid_errors`

#### Scenario: Deleting one row out of several leaves the rest intact

- GIVEN a leaf holding more than one row
- WHEN one row's rowid is deleted
- THEN the remaining rows MUST stay in ascending rowid order, unchanged

**Tests:** `src/btree/delete.rs::tests::deleting_one_of_two_rows_keeps_the_other`, `tests/corpus/btree_delete_test.rs::delete_single_row_from_a_two_row_leaf`

#### Scenario: Bulk delete stays oracle-identical at scale

- GIVEN 1000 rows inserted, then every other rowid deleted one at a time
- WHEN the file is reopened by stock `sqlite3`
- THEN `PRAGMA integrity_check` MUST pass and every surviving row MUST read back identically, in rowid order

**Tests:** `tests/corpus/btree_delete_test.rs::bulk_delete_every_other_row_out_of_1000`

#### Scenario: Deleting a row with an overflow payload frees its overflow chain

- GIVEN a row whose payload spilled into one or more overflow pages (Requirement 2)
- WHEN that row is deleted
- THEN every page in its overflow chain MUST be deallocated via the pager's freelist (Requirement per spec 007/freelist), and a later insert MUST reuse that freed space rather than growing the file

**Tests:** `tests/corpus/btree_delete_test.rs::deleting_a_row_with_an_overflow_payload_frees_its_overflow_pages`

### Requirement 13: Page Merge/Collapse on Underflow [MUST]

When a delete leaves a non-root page with zero cells, the system MUST remove that page's routing entry from its parent (redirecting the parent's `rightmost` pointer if the emptied page was the parent's rightmost child) and deallocate the emptied page via the pager's freelist (Requirement per spec 007/freelist). This is a documented simplification of SQLite's proactive half-full-threshold sibling redistribution: pages are only collapsed once completely empty, not proactively rebalanced while still holding rows — sufficient for structural validity (`PRAGMA integrity_check`) and for freed pages to be reused by a later insert, without porting the exact 3-sibling balance algorithm.

**Implementation:** `src/btree/delete.rs::collapse_into_ancestors`

#### Scenario: Deleting the only row in the root leaves an empty root leaf

- GIVEN a table with exactly one row, at a root page that is itself a leaf
- WHEN that row is deleted
- THEN the root page MUST remain a valid, empty leaf page (the root can never be deallocated)

**Tests:** `src/btree/delete.rs::tests::deleting_the_only_row_leaves_an_empty_root_leaf`

#### Scenario: Page merge triggers correctly when a non-root leaf empties

- GIVEN a table b-tree with more than one leaf page (a prior split), all rows in one leaf deleted
- WHEN the last row in that leaf is deleted
- THEN the leaf MUST be removed from its parent's routing entries and deallocated, and stock `sqlite3` MUST still pass `PRAGMA integrity_check`

**Tests:** `tests/corpus/btree_delete_test.rs::delete_triggers_page_collapse_across_a_split_boundary`

### Requirement 14: Underflow Cascading to Root [MUST]

When collapsing a page leaves its own parent with zero routing entries (just a `rightmost` pointer), the system MUST cascade the collapse up the ancestor chain. If the cascade reaches the root itself, the system MUST relocate the sole remaining child's content (leaf or interior, verbatim) into the fixed root page in place, then deallocate the now-vacated child page — mirroring `insert.rs::root_split` in reverse.

**Implementation:** `src/btree/delete.rs::collapse_into_ancestors`, `src/btree/delete.rs::collapse_root`

#### Scenario: Delete-all cascades every level back to a single empty leaf root

- GIVEN enough rows inserted to force at least one leaf split, then every row deleted
- WHEN the last row is deleted
- THEN the tree MUST cascade-collapse back to a single empty leaf at the root page, and stock `sqlite3` MUST report zero rows and pass `PRAGMA integrity_check`

**Tests:** `tests/corpus/btree_delete_test.rs::delete_all_rows_leaves_an_empty_table`

#### Scenario: Round-trip insert → delete → insert reuses freed pages

- GIVEN rows inserted (forcing a split), then all deleted, then the same rows reinserted
- WHEN the reinsert completes
- THEN the file's page count MUST NOT grow past what the first insert pass produced (freed pages are reused via the freelist), and stock `sqlite3` MUST pass `PRAGMA integrity_check`

**Tests:** `tests/corpus/btree_delete_test.rs::round_trip_insert_delete_insert_reuses_freed_pages`
