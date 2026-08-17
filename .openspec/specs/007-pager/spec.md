---
domain: storage
version: 0.1.0
status: draft
date: 2026-08-14
---

# 007 — Pager

The page-access layer between the VFS and the b-tree cursor: resolves page numbers to bytes, refuses to serve a database with a hot rollback journal, and (Requirement 2) merges in committed WAL frames for databases with an uncheckpointed `-wal` file. Backs V1 step 2 (#35) and step 6 (#36), part of epic #5 phase 3. Depends on spec 003 (VFS, header) and spec 006 (the `PageSource` trait `TableCursor`/`IndexCursor` already consume generically — this spec's `Pager` implements it directly, so neither cursor changes). Refs: 001/Req-4.

Everything in this spec is **Tier 0 READ CORE** — never droppable.

## Philosophy

`Pager` replaces `VfsPageSource` (spec 003) as the page source `TableCursor`/`IndexCursor` are built against, adding exactly two things `VfsPageSource` doesn't: hot-journal refusal (Requirement 1) and WAL-frame merging (Requirement 2). Everything else — page fetch, page size, reserved bytes — is unchanged, by construction: `Pager` wraps a `VfsPageSource` internally rather than reimplementing page reads. `src/pager/` is not exempt from the `mvl-limit` qualified-subset gate (no `unsafe`/`dyn`/explicit lifetimes); `Pager::open` takes its `Vfs` generically (`<V: Vfs>`), never as `&dyn Vfs`, so the trait-object boundary stays inside `src/vfs/` where it's already accounted for. That generic-parameter shape is the reference pattern for keeping the boundary contained; the VDBE deliberately keeps a second, narrower `dyn` boundary instead (`Rc<dyn PageSource>` in `src/vdbe/{exec,cursor}.rs`, exempted since #90). ADR-0013 considered making `Vm` generic over `P: PageSource` (#114) and rejected it: a `Vm` shares one page source across N open cursors via cheap `Rc` clones, so genericizing would force the no-database constructor path (`Vm::new()`) to name a concrete `P`, drop `Vm`'s `Default` derive, and thread `<P: PageSource>` through every opcode handler — trading a justified trait object for pervasive generic noise without changing runtime dispatch, since real callers would still instantiate `Vm<Rc<dyn PageSource>>`.

Locking is out of scope for both requirements below. Spike 005 (#8, closed) validated the *approach* — byte-identical `fcntl` locks (journal-mode lock ladder and WAL `-shm` reader-mark protocol) genuinely interoperate with a live stock `sqlite3` process — but implementing it is separate, larger scope than a page-view layer and is tracked as a follow-up (#45), deferred per #35/#36's own acceptance criteria rather than blocking on it. `Pager` takes no file locks.

## Requirements

### Requirement 1: Hot-Journal Detection [MUST]

The system MUST refuse to open a database that has an adjacent `-journal` file with a valid rollback-journal header, rather than risk serving pre-rollback (uncommitted) pages as committed data. A `-journal` file that exists but does not start with the rollback-journal magic (e.g. zeroed by `PRAGMA journal_mode=PERSIST`'s post-commit reset, or too short to hold a full header) is not hot and MUST NOT block opening.

**Implementation:** `src/pager.rs::Pager::open`

**Tests:** inline `#[cfg(test)]` in `src/pager.rs`

**Corpus:** `tests/corpus/fixtures/journalstates/`

#### Scenario: Hot journal refuses to open

- GIVEN `journalstates/hot_journal.db` and its adjacent `hot_journal.db-journal` (a rollback-journal writer that spilled uncommitted pages into the main file before being interrupted — see `tools/gen_fixtures.sh`)
- WHEN `Pager::open` is called
- THEN it returns `PagerError::HotJournal`, before any page is read

**Tests:** `src/pager.rs::tests::fixtures::hot_journal_fixture_is_refused`

#### Scenario: Cold or absent journal opens cleanly

- GIVEN a database with no `-journal` file, or one that exists but is zeroed/too short to hold a valid header
- WHEN `Pager::open` is called
- THEN it succeeds

**Tests:** `src/pager.rs::tests::no_journal_opens_cleanly`, `src/pager.rs::tests::zeroed_persist_mode_journal_is_not_hot`, `src/pager.rs::tests::empty_journal_file_is_not_hot`, `src/pager.rs::tests::short_journal_file_is_not_hot`

### Requirement 2: Page-View Abstraction, Zero Behavior Change [MUST]

`Pager` MUST implement the same `PageSource` trait `VfsPageSource` does, so `TableCursor<Pager>` / `IndexCursor<Pager>` produce byte-identical results to `TableCursor<VfsPageSource>` / `IndexCursor<VfsPageSource>` on every fixture that has no hot journal and no pending WAL — including auto-vacuum databases, where the b-tree cursor's pointer-following traversal never visits the interleaved pointer-map pages directly and therefore needs no pointer-map-specific logic in `Pager`.

**Implementation:** `src/pager.rs::Pager` (`impl PageSource for Pager`)

**Tests:** inline `#[cfg(test)]` in `src/pager.rs`

**Corpus:** `tests/corpus/fixtures/btrees/`, `tests/corpus/fixtures/features/autovacuum.db`

#### Scenario: At-rest fixture unchanged

- GIVEN `btrees/table_single_page.db`
- WHEN read through `TableCursor<Pager>` instead of `TableCursor<VfsPageSource>`
- THEN the decoded rows are identical

**Tests:** `src/pager.rs::tests::fixtures::table_single_page_fixture_reads_identically_through_pager`

#### Scenario: Auto-vacuum fixture unaffected by pointer-map page

- GIVEN `features/autovacuum.db`, whose table `t` root page is discovered via `read_schema` (never hardcoded, since the interleaved pointer-map page can shift it)
- WHEN read through `TableCursor<Pager>`
- THEN the row decodes identically to the non-auto-vacuum case

**Tests:** `src/pager.rs::tests::fixtures::autovacuum_fixture_reads_identically_through_pager`

### Requirement 3: WAL Frame Reading (Read-Only Recovery) [MUST]

For a WAL-mode database with a non-empty, sub-header-length-or-larger `-wal` file, `Pager` MUST parse the WAL header (both checksum-endianness variants — magic `0x377f0682` is native byte order, the common case, `0x377f0683` is always big-endian, per spike #7 finding 2), walk its frames validating checksum and salt, and overlay the latest committed frame per page over the main file's pages. A frame whose salts don't match the header's, or whose checksum doesn't verify, ends the scan (not an error — a torn tail is the normal shape of a WAL file mid-write); everything published by an earlier commit in the same scan survives. No `-shm` file is read — this is quiescent, read-only recovery only (live-writer coexistence is validated as needed by spike 005/#8, closed; implementation tracked as #45). A malformed WAL *header* (bad magic, too short, bad checksum, or a page size that doesn't match the main database's) MUST return `PagerError::Wal`, never panic; a missing or empty `-wal` file (the common case: a fully checkpointed WAL) is not an error and yields no overlay.

**Implementation:** `src/pager/wal.rs`, `src/pager.rs::read_wal_pages`

**Tests:** inline `#[cfg(test)]` in `src/pager/wal.rs` and `src/pager.rs`

**Corpus:** `tests/corpus/fixtures/journalstates/`

#### Scenario: Uncheckpointed WAL rows are visible

- GIVEN `journalstates/wal_pending.db` and its adjacent `wal_pending.db-wal` (three separate commits to the same page, none checkpointed into the main file)
- WHEN read through `TableCursor<Pager>`
- THEN all three rows are visible, matching `sqlite3`

**Tests:** `src/pager.rs::tests::fixtures::wal_pending_fixture_shows_uncheckpointed_rows`

#### Scenario: Both checksum-endianness paths decode identically

- GIVEN `wal_pending_bigendian.db-wal` — `wal_pending.db-wal`'s content with the magic flipped to `0x377f0683` and every checksum independently recomputed in big-endian arithmetic
- WHEN read through `TableCursor<Pager>`
- THEN the decoded rows are identical to `wal_pending.db`'s

**Tests:** `src/pager.rs::tests::fixtures::wal_pending_bigendian_fixture_decodes_identically`, `src/pager/wal.rs::tests::native_checksum_header_parses`, `src/pager/wal.rs::tests::bigendian_checksum_header_parses`

#### Scenario: Stale foreign-generation frame is rejected

- GIVEN `wal_pending_stale.db-wal`, which has a committed frame from an unrelated WAL generation (different salts) appended after its own last commit
- WHEN read through `TableCursor<Pager>`
- THEN only the two rows from this generation's own commits are visible — the foreign frame's row never surfaces

**Tests:** `src/pager.rs::tests::fixtures::wal_pending_stale_fixture_rejects_foreign_frame`, `src/pager/wal.rs::tests::stale_foreign_frame_is_rejected_on_salt_mismatch`

#### Scenario: Trailing spilled-but-uncommitted frames are ignored

- GIVEN `wal_pending_trailing.db-wal`, where a big transaction spilled dirty pages into the WAL as non-commit frames before rolling back
- WHEN read through `TableCursor<Pager>`
- THEN only the pre-existing committed row is visible — none of the rolled-back insert

**Tests:** `src/pager.rs::tests::fixtures::wal_pending_trailing_fixture_shows_only_committed_row`, `src/pager/wal.rs::tests::trailing_spilled_frames_are_ignored`

#### Scenario: Malformed WAL never panics

- GIVEN arbitrary malformed byte sequences in place of a `-wal` file's contents
- WHEN parsed
- THEN `WalHeader::parse` and `committed_pages` return errors or empty results, never panic (fuzz target)

**Tests:** `src/pager/wal.rs::tests::too_short_is_err_not_panic`, `src/pager/wal.rs::tests::bad_magic_is_err`, `src/pager/wal.rs::tests::corrupted_header_checksum_is_err`, `src/pager/wal.rs::tests::garbage_input_never_panics`, `tests/fuzz/fuzz_targets/wal_frames.rs`

### Requirement 4: Dirty Page Tracking and Flush [MUST]

Unlike Requirements 1-3, this requirement is **Tier 2 WRITE CORE** (V3 phase 1, epic #161, #166) — the pager's first write-path capability, added on top of the read-only Requirements 1-3 above without changing their behavior. `Pager::get_page_mut` MUST return a mutable buffer for a page, transparently reading it first (through the same WAL-overlay-then-file precedence `read_page` already uses) the first time it's requested since the last flush, and caching it as dirty. `Pager::read_page` MUST consult this dirty cache ahead of the WAL overlay, so a page mutated via `get_page_mut` reads back its new bytes immediately through the same `Pager`, even before `flush` runs. `Pager::flush` MUST write every dirty page back to the underlying file (in ascending page-number order — a deterministic default, not a correctness requirement of this requirement alone, since no partial-flush recovery exists in this pager yet; see #172), `fsync` it, and clear the dirty set.

Writing to the underlying file requires a read-write file handle. Rather than open a second file descriptor to the same path alongside the existing read-only one — which would trip the documented "`close()` drops all `fcntl` locks held on that inode, regardless of which fd acquired them" hazard (this spec's Requirement 1 doc, #45) before #45's per-inode fd-cache exists to guard against it — `Pager::open` now acquires its single file handle via the new `Vfs::open_write`/`VfsFile::write_at`/`VfsFile::sync` surface (`WritablePageSource`, `src/vfs/page_source.rs`) instead of `Vfs::open_read`. Every other read-only `PageSource` consumer (`VfsPageSource` itself, used directly by non-`Pager` cursors) is unaffected: it keeps using `Vfs::open_read`, never asking a genuinely read-only filesystem for write access it doesn't need.

**Implementation:** `src/pager.rs::Pager::get_page_mut`, `src/pager.rs::Pager::flush`, `src/vfs/page_source.rs::WritablePageSource`

**Tests:** inline `#[cfg(test)]` in `src/pager.rs` and `src/vfs.rs`

**Corpus:** `tests/corpus/pager_write_test.rs` (shells out to a real, writable `sqlite3`-created fixture rather than a committed corpus file, since this requirement's whole point is writing)

#### Scenario: A mutated page reads back immediately, and again after flush

- GIVEN an open `Pager` over a two-page database
- WHEN `get_page_mut(2)` is called and its returned buffer is overwritten, then `read_page(2)` is called before `flush`, then again after `flush`
- THEN both reads return the new bytes, the untouched page 1 is unaffected, and a freshly-opened `Pager` over the same file also sees the new bytes on page 2 and the original bytes on page 1

**Tests:** `src/pager.rs::tests::get_page_mut_then_flush_roundtrips`, `src/pager.rs::tests::flush_with_no_dirty_pages_is_a_no_op`

#### Scenario: A flushed page still opens in stock `sqlite3`

- GIVEN a database created by stock `sqlite3` with one table and one row
- WHEN a page is fetched via `get_page_mut`, written back unchanged, and flushed
- THEN stock `sqlite3` still opens the file, `PRAGMA integrity_check` reports `ok`, and the original row reads back unchanged — the compatibility proof this whole epic (#161) is gated on

**Tests:** `tests/corpus/pager_write_test.rs::flushed_page_still_opens_and_integrity_checks_in_stock_sqlite3`

#### Scenario: Both VFS backends satisfy the write contract

- GIVEN a file opened via `Vfs::open_write`
- WHEN bytes are written at an offset and synced
- THEN a fresh `Vfs::open_read` handle on the same path reads back the new bytes — true for both `UnixVfs` and `MemoryVfs`

**Tests:** `src/vfs.rs::tests::memory_vfs_contract`, `src/vfs.rs::tests::unix_vfs_contract`

### Requirement 5: Freelist Allocate/Deallocate [MUST]

Tier 2 WRITE CORE (V3 phase 1, epic #161, #167), built on top of Requirement 4's dirty-page-tracking/flush primitives. `Pager::allocate_page` MUST return a free page number: popping one off the freelist (a leaf page number from the current trunk page's leaf array, or the trunk page itself once its leaf array is empty, promoting the trunk's own next-trunk pointer) when the freelist is non-empty, or extending the database by one page (incrementing the header's page-count field) when it is empty. `Pager::deallocate_page` MUST return a page to the freelist: appended to the current trunk page's leaf array if it has room (`(page_size - 8) / 4` entries), or made the new trunk page itself (pointing at the old trunk) once the current trunk is full. Both operations MUST update the header's freelist-trunk-page and freelist-page-count fields (bytes 32-35 and 36-39) on page 1 in the same call, via `Pager::get_page_mut`, so the bookkeeping is flushed atomically with the allocation/deallocation on the next `Pager::flush`.

Refs: 003/Req-2, 007/Req-4.

**Implementation:** `src/pager.rs::Pager::allocate_page`, `src/pager.rs::Pager::deallocate_page`, `src/pager/freelist.rs::TrunkPage`

**Tests:** inline `#[cfg(test)]` in `src/pager.rs` and `src/pager/freelist.rs`

**Corpus:** `tests/corpus/pager_write_test.rs`

#### Scenario: Allocation extends the file when the freelist is empty

- GIVEN a `Pager` over a database with an empty freelist
- WHEN `allocate_page` is called
- THEN it returns `page_count + 1` and the header's page-count field is incremented; no freelist field changes

**Tests:** `src/pager.rs::tests::allocate_with_empty_freelist_extends_file`

#### Scenario: Deallocate then allocate round-trips a single page

- GIVEN a `Pager` over a database with an empty freelist
- WHEN a page is deallocated (becoming the sole trunk page) and then allocated again
- THEN the same page number is returned, and the header's freelist-trunk-page and freelist-page-count fields return to their original (empty) values

**Tests:** `src/pager.rs::tests::deallocate_then_allocate_round_trips_single_page`

#### Scenario: Trunk leaves fill before a new trunk is chained

- GIVEN a `Pager` whose freelist has one trunk page with room for more leaves
- WHEN additional pages are deallocated
- THEN they append to the existing trunk's leaf array, and `allocate_page` pops leaves before ever consuming the trunk page itself; once a trunk is full, the next deallocation chains a new trunk page pointing at the old one

**Tests:** `src/pager.rs::tests::deallocate_appends_to_existing_trunk_leaves`, `src/pager.rs::tests::deallocate_overflows_into_new_trunk_when_full`

#### Scenario: Allocate/deallocate round trip still integrity-checks in stock `sqlite3`

- GIVEN a database created by stock `sqlite3` with one table and one row
- WHEN a page is allocated (extending the file), flushed, then deallocated (returned to the freelist), and flushed again
- THEN stock `sqlite3` still opens the file, `PRAGMA integrity_check` reports `ok`, `PRAGMA freelist_count` reports `1`, and the original row reads back unchanged

**Tests:** `tests/corpus/pager_write_test.rs::allocate_then_deallocate_page_still_integrity_checks_in_stock_sqlite3`
