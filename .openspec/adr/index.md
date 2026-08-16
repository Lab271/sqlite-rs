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
