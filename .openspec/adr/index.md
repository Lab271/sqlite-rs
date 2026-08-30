# ADR Index

Specs record what the system must do; ADRs record **why it is shaped this way** — the forks in the road, with rejected alternatives and consequences. ADRs are immutable once accepted; supersede, don't edit.

| # | Title | Date |
|---|-------|------|
| [0001](0001-shm-access-pread-not-mmap.md) | `-shm` access via pread/pwrite, not mmap | 2026-08-15 |
| [0002](0002-value-blocks-over-layers.md) | Value blocks over layer-ordered development | 2026-08-13 |
| [0003](0003-tier-model-read-completeness-first.md) | Tier model: read-completeness before any SQL | 2026-08-13 |
| [0004](0004-compatibility-contract-three-surfaces.md) | Compatibility contract: format + dialect + locking, not internals | 2026-08-13 |
| [0005](0005-pinned-oracle.md) | Pinned non-codec sqlite3 as sole correctness authority | 2026-08-14 |
| [0006](0006-one-minor-per-phase.md) | Versioning: one minor per completed plan phase | 2026-08-14 |
| [0007](0007-expressions-as-opcodes-kernel.md) | Expressions as opcodes; value-semantics kernel; no evaluator | 2026-08-15 |
| [0008](0008-spikes-disposable-code-surviving-evidence.md) | Spike discipline: disposable code, surviving evidence | 2026-08-15 |
| [0009](0009-zero-unsafe.md) | Zero unsafe: safe syscall wrappers (generalizes 0001) | 2026-08-15 |
| [0010](0010-deterministic-inventories.md) | Deterministic inventories over estimates | 2026-08-15 |
| [0011](0011-strict-metrics-declared-vs-delivered.md) | Strict metrics: declared is not delivered | 2026-08-14 |
| [0012](0012-ephemeral-tables-in-memory.md) | Ephemeral tables: opcode semantics, in-memory backing | 2026-08-15 |
| [0013](0013-vdbe-dyn-pagesource-boundary.md) | VDBE keeps a second `dyn` boundary for `PageSource` (rejects generic `Vm`) | 2026-08-16 |
| [0014](0014-expr-depth-bound-diverges-from-sqlite.md) | Expression nesting bound stays at 200, not SQLite's 1000 | 2026-08-16 |
| [0015](0015-variable-opcode-reopens-frozen-set.md) | `Variable` reopens the frozen V2 opcode set | 2026-08-16 |
| [0016](0016-hot-journal-auto-recovery.md) | Hot-journal recovery is automatic in `Pager::open`, not a separate opt-in call | 2026-08-18 |
| [0017](0017-writable-vm-shares-pager-via-refcell-pagesource.md) | A writable `Vm` shares one `Pager` via `RefCell<Pager>: PageSource` | 2026-08-19 |
| [0018](0018-copy-opcode-reopens-frozen-set.md) | `Copy` (and `AggStep`/`AggFinal`) reopen the frozen V2 opcode set | 2026-08-21 |
| [0019](0019-aggstep-p4-collation-and-p5-reset.md) | `AggStep` gains a collation-carrying `P4` and a `P5` reset flag | 2026-08-21 |
| [0020](0020-index-ordered-scan-opcodes-reopen-frozen-set.md) | `IdxRewind`/`IdxLast`/`IdxNext`/`IdxPrev` reopen the frozen V2 opcode set | 2026-08-21 |
| [0021](0021-correlated-subquery-rematerialization-cost.md) | Defer a coroutine rewrite for correlated subqueries; scope an incremental hoist instead | 2026-08-21 |
| [0022](0022-pager-has-no-page-cache.md) | `Pager` has no page cache — repeated-seek workloads pay full I/O every row | 2026-08-21 |
| [0023](0023-leaf-cell-splice.md) | In-place leaf/index cell splice with real freeblock bookkeeping | 2026-08-22 |
| [0024](0024-hot-journal-recovery-reserved-probe-single-fd.md) | Hot-journal recovery probes RESERVED and shares one fd with `Pager` | 2026-08-22 |
| [0025](0025-passive-only-checkpoint-linear-frame-scan.md) | PASSIVE-only checkpoint, single non-blocking lock, linear frame scan | 2026-08-23 |
| [0026](0026-wal-writer-reopens-and-rescans-per-flush.md) | WAL writer reopens and rescans the `-wal` file on every flush | 2026-08-23 |
| [0027](0027-correlated-subquery-seek-not-cache.md) | Correlated scalar subquery equality compiles to a seek, not a cache lookup | 2026-08-23 |
| [0028](0028-pager-page-cache-landed.md) | `Pager` page cache landed — supersedes ADR-0022's problem statement | 2026-08-25 |
| [0029](0029-introspection-pragmas-outside-vdbe.md) | Read-only introspection pragmas live outside the VDBE, as CLI-layer synthetic result sets | 2026-08-25 |
| [0030](0030-zero-proc-macro-dependencies.md) | Zero proc-macro dependencies: hand-rolled errors and readline | 2026-08-26 |
| [0031](0031-vendor-nix-subset.md) | Vendor a `nix` subset: reintroduce a single, narrow `unsafe` boundary | 2026-08-26 |
| [0032](0032-hash-group-by-second-strategy.md) | Hash `GROUP BY` is a second strategy with its own opcode family, and still emits groups in key order | 2026-08-27 |
| [0033](0033-constant-propagation-and-or-to-in-extend-fast-paths-in-place.md) | Constant propagation and OR-to-IN extend existing equality fast paths in place; only genuine range seeks wait for a new opcode | 2026-08-28 |
| [0034](0034-index-range-seeks.md) | Real-index range seeks for `BETWEEN`/`IN`/`LIKE`-prefix (`SeekIndexGE`/`IdxCompareGT`) | 2026-08-28 |
| [0035](0035-wal-resume-hint-cache-supersedes-0026.md) | `Pager`-cached WAL resume hint supersedes ADR-0026's per-flush rescan | 2026-08-29 |
| [0036](0036-pragma-synchronous-fsync-policy.md) | `PRAGMA synchronous` fsync-skip policy, and why `SynchronousMode` lives in `header.rs` | 2026-08-29 |
| [0037](0037-macos-plain-fsync-not-fullfsync.md) | On macOS, `Vfs::sync` calls plain `fsync(2)`, not `std`'s `F_FULLFSYNC` | 2026-08-30 |
