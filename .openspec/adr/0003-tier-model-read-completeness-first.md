# 0003 — Tier model: read-completeness before any SQL

**Status:** Accepted · **Date:** 2026-08-13

## Context

A drop-order exercise ("if you had to cut features, what goes last?") surfaced the asymmetry: reading a feature's data is much smaller than supporting the feature — WITHOUT ROWID, STRICT, and FTS5 shadow tables are all just b-trees with records on disk.

## Decision

Four tiers by droppability. **Tier 0 READ CORE (never droppable):** whatever wrote the file, sqlite-rs reads the data out — all serial types, all encodings, index b-trees, overflow, WAL frame reading, hot-journal detection, graceful unknowns. The storage layer goes 100% format-complete before any SQL feature is scoped, and Tier 0 has zero dependency on the grammar (`sqlite_master` decoded by a minimal DDL reader, spec 002 Req-5).

## Alternatives rejected

- Feature-by-feature completeness (read+write+semantics per feature): drops whole tables when a feature is descoped.
- Parser-first bootstrap: puts the ~200-production grammar inside the never-droppable core.

## Consequences

WAL, WITHOUT ROWID, STRICT, generated columns each appear twice in the plan (read = Tier 0, semantics = Tier 3). The reader ships standalone forever; tier conformance tests (tests/tiers/) encode the contract; tier0.rs may never contain an `#[ignore]`.
