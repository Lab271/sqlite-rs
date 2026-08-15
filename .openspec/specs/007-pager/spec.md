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

`Pager` replaces `VfsPageSource` (spec 003) as the page source `TableCursor`/`IndexCursor` are built against, adding exactly two things `VfsPageSource` doesn't: hot-journal refusal (Requirement 1) and WAL-frame merging (Requirement 2). Everything else — page fetch, page size, reserved bytes — is unchanged, by construction: `Pager` wraps a `VfsPageSource` internally rather than reimplementing page reads. `src/pager/` is not exempt from the `mvl-limit` qualified-subset gate (no `unsafe`/`dyn`/explicit lifetimes) — only `src/vfs/` is; `Pager::open` takes its `Vfs` generically (`<V: Vfs>`), never as `&dyn Vfs`, so the trait-object boundary stays inside `src/vfs/` where it's already accounted for.

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
