# 0009 — Zero unsafe: safe syscall wrappers and pread over mmap

**Status:** Accepted · **Date:** 2026-08-15 (supersedes the fenced-boundary policy of 0.3.0)

*(Generalizes ADR-0001, which records the `-shm` pread-not-mmap sub-decision in detail.)*

## Context

Safe-reader locking initially introduced `deny(unsafe_code)` with a scoped allow in `src/vfs/` (raw libc fcntl; mmap of the `-shm` wal-index). The fence concentrated the unsafe — and mmap carried a documented SIGBUS limitation: uncatchable process death if another process truncates the mapping.

## Decision

Eliminate rather than fence (#66): fcntl byte-range locks via nix's safe wrappers; the `-shm` wal-index read/written via `FileExt::read_at`/`write_at` on the file (page cache keeps this coherent with sqlite3's MAP_SHARED mapping on the same machine); fork-based tests replaced by spawned processes. `#![forbid(unsafe_code)]` crate-wide. Remaining FFI unsafe lives only in std/nix — the universal Rust trust base.

## Alternatives rejected

- Keeping the fenced boundary (C-class bugs could still live there; the CVE assessment named it the honest remainder).
- SIGBUS handler over the mmap (global signal state, still an abort model).

## Consequences

The SIGBUS failure mode ceased to exist — a truncated `-shm` yields `Err`, a strictly better failure mode than stock SQLite's own mmap. "Zero unsafe in this codebase" is literally true and machine-checked (mvl-limit, geiger).
