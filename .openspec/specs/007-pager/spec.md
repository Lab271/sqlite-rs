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

Locking (spike 004, #8) is out of scope for both requirements below — #8 is still open, and both #35 and #36's acceptance criteria explicitly allow deferring the locking decision with rationale rather than blocking on it. `Pager` takes no file locks.

## Requirements

### Requirement 1: Hot-Journal Detection [MUST]

The system MUST refuse to open a database that has an adjacent `-journal` file with a valid rollback-journal header, rather than risk serving pre-rollback (uncommitted) pages as committed data. A `-journal` file that exists but does not start with the rollback-journal magic (e.g. zeroed by `PRAGMA journal_mode=PERSIST`'s post-commit reset, or too short to hold a full header) is not hot and MUST NOT block opening.

**Implementation:** `src/pager/mod.rs::Pager::open`

**Tests:** inline `#[cfg(test)]` in `src/pager/mod.rs`

**Corpus:** `tests/corpus/fixtures/journalstates/`

#### Scenario: Hot journal refuses to open

- GIVEN `journalstates/hot_journal.db` and its adjacent `hot_journal.db-journal` (a rollback-journal writer that spilled uncommitted pages into the main file before being interrupted — see `tools/gen_fixtures.sh`)
- WHEN `Pager::open` is called
- THEN it returns `PagerError::HotJournal`, before any page is read

**Tests:** `src/pager/mod.rs::tests::fixtures::hot_journal_fixture_is_refused`

#### Scenario: Cold or absent journal opens cleanly

- GIVEN a database with no `-journal` file, or one that exists but is zeroed/too short to hold a valid header
- WHEN `Pager::open` is called
- THEN it succeeds

**Tests:** `src/pager/mod.rs::tests::no_journal_opens_cleanly`, `src/pager/mod.rs::tests::zeroed_persist_mode_journal_is_not_hot`, `src/pager/mod.rs::tests::empty_journal_file_is_not_hot`, `src/pager/mod.rs::tests::short_journal_file_is_not_hot`

### Requirement 2: Page-View Abstraction, Zero Behavior Change [MUST]

`Pager` MUST implement the same `PageSource` trait `VfsPageSource` does, so `TableCursor<Pager>` / `IndexCursor<Pager>` produce byte-identical results to `TableCursor<VfsPageSource>` / `IndexCursor<VfsPageSource>` on every fixture that has no hot journal and no pending WAL — including auto-vacuum databases, where the b-tree cursor's pointer-following traversal never visits the interleaved pointer-map pages directly and therefore needs no pointer-map-specific logic in `Pager`.

**Implementation:** `src/pager/mod.rs::Pager` (`impl PageSource for Pager`)

**Tests:** inline `#[cfg(test)]` in `src/pager/mod.rs`

**Corpus:** `tests/corpus/fixtures/btrees/`, `tests/corpus/fixtures/features/autovacuum.db`

#### Scenario: At-rest fixture unchanged

- GIVEN `btrees/table_single_page.db`
- WHEN read through `TableCursor<Pager>` instead of `TableCursor<VfsPageSource>`
- THEN the decoded rows are identical

**Tests:** `src/pager/mod.rs::tests::fixtures::table_single_page_fixture_reads_identically_through_pager`

#### Scenario: Auto-vacuum fixture unaffected by pointer-map page

- GIVEN `features/autovacuum.db`, whose table `t` root page is discovered via `read_schema` (never hardcoded, since the interleaved pointer-map page can shift it)
- WHEN read through `TableCursor<Pager>`
- THEN the row decodes identically to the non-auto-vacuum case

**Tests:** `src/pager/mod.rs::tests::fixtures::autovacuum_fixture_reads_identically_through_pager`
