---
domain: architecture
version: 0.1.0
status: draft
date: 2026-08-13
---

# 001 — Architecture

sqlite-rs is a pure Rust implementation of SQLite. This spec defines the system breakdown, module boundaries, and implementation scope.

## Philosophy

SQLite's architecture is strictly layered. Each layer has one job. The pager does not know what a B-tree is. The B-tree does not know what SQL means. We preserve this separation.

**Design principles:**

1. **Compatibility over elegance** — match SQLite behavior, even when Rust would do it differently
2. **Layer isolation** — modules communicate through defined interfaces
3. **Test against the oracle** — SQLite's test suite is the spec; divergence is a bug
4. **Incremental delivery** — each layer can be tested independently
5. **Read-completeness first** — the storage layer is 100% format-complete before any SQL feature is scoped; whatever wrote the file, sqlite-rs reads the data out

## Tier Model

Capabilities are tiered by droppability (see [plan.md](../../plan.md#core-definition--drop-order) for the full definition and drop order):

| Tier | Scope | Droppable? |
|------|-------|------------|
| **Tier 0 — READ CORE** | Feature-agnostic storage reading: all serial types, all text encodings, table + index b-trees, overflow chains, WAL frame reading, hot-journal detection, graceful unknowns | **Never** |
| **Tier 1 — QUERY CORE** | Single-table SELECT, core scalar functions, affinity, built-in collations | Planner droppable (full scans are correct); SELECT core is not |
| **Tier 2 — WRITE CORE** | CRUD on rowid tables, basic constraints, rollback-journal transactions, `integrity_check`-clean output | Simplifiable, not droppable |
| **Tier 3** | Everything else | Yes, in defined drop order |

**Layer-to-tier mapping:**

| Layer | Tier 0 share | Notes |
|-------|--------------|-------|
| VFS | Read path | Write path is Tier 2 |
| Pager | Read path, WAL frame reading, hot-journal detection | Journaling/locking writes are Tier 2; WAL *writing* Tier 3 |
| B-Tree | Full cursor read incl. index b-trees | Insert/delete/balance are Tier 2 |
| Record format | Complete (all serial types, all encodings) | Entirely Tier 0 |
| Tokenizer/Parser | **Not in Tier 0** — `sqlite_master` uses a minimal DDL reader | Full parser starts at Tier 1 |
| VDBE / Codegen | None | Tier 1 upward |
| SQL Interface | Raw-dump API only | Full API Tier 1+ |

The asymmetry to preserve: WAL, WITHOUT ROWID, STRICT, and STORED generated columns appear twice — *reading* their data is Tier 0, *executing* their semantics is Tier 3.

## System Layers

### Layer 1: SQL Interface

Public API for database operations.

| Component | Responsibility | SQLite equivalent |
|-----------|----------------|-------------------|
| `Connection` | Database handle, statement cache | `sqlite3*` |
| `Statement` | Prepared statement | `sqlite3_stmt*` |
| `Value` | Dynamic SQL value (NULL, INTEGER, REAL, TEXT, BLOB) | `sqlite3_value*` |
| `Row` | Result row iterator | `sqlite3_step()` |
| `Error` | Error codes and messages | `SQLITE_*` codes |

**Implementation:** `src/lib.rs`, `src/connection.rs`, `src/statement.rs`

**Estimated lines:** ~2,000

### Layer 2: Frontend (Tokenizer + Parser + Analyzer)

Transforms SQL text into an analyzed AST ready for code generation.

| Component | Responsibility | SQLite equivalent |
|-----------|----------------|-------------------|
| `Tokenizer` | Lexical analysis | `tokenize.c` |
| `Parser` | Grammar parsing (LALR(1)) | `parse.y` + Lemon |
| `AST` | Syntax tree nodes | `parse.c` output |
| `Analyzer` | Name resolution, type checking | `resolve.c`, `expr.c` |

**Implementation:** `src/parser/`

**Estimated lines:** ~8,000

**See:** [002-parser](../002-parser/spec.md) for detailed grammar spec.

### Layer 3: Code Generator

Compiles analyzed AST to VDBE bytecode.

| Component | Responsibility | SQLite equivalent |
|-----------|----------------|-------------------|
| `Planner` | Query optimization, index selection | `where.c`, `whereexpr.c` |
| `SelectCompiler` | SELECT → bytecode | `select.c` |
| `InsertCompiler` | INSERT → bytecode | `insert.c` |
| `UpdateCompiler` | UPDATE → bytecode | `update.c` |
| `DeleteCompiler` | DELETE → bytecode | `delete.c` |
| `DdlCompiler` | DDL (CREATE, DROP, ALTER) → bytecode | `build.c` |
| `ExprCompiler` | Expression → bytecode | `expr.c` |

**Implementation:** `src/codegen/`

**Estimated lines:** ~35,000

**Key files in SQLite:**

| File | Lines | Focus |
|------|-------|-------|
| `select.c` | ~7,000 | SELECT compilation |
| `where.c` | ~5,000 | Query planning |
| `wherecode.c` | ~3,000 | WHERE bytecode |
| `whereexpr.c` | ~2,000 | WHERE expressions |
| `expr.c` | ~6,000 | Expression handling |
| `insert.c` | ~2,500 | INSERT |
| `update.c` | ~1,500 | UPDATE |
| `delete.c` | ~1,200 | DELETE |
| `build.c` | ~5,000 | DDL |

### Layer 4: Virtual Machine (VDBE)

Executes bytecode programs.

| Component | Responsibility | SQLite equivalent |
|-----------|----------------|-------------------|
| `Opcode` | Instruction enum (192 opcodes) | `opcodes.h` |
| `Program` | Bytecode array | `Vdbe` |
| `Interpreter` | Fetch-decode-execute loop | `vdbe.c` |
| `Mem` | Register/value storage | `vdbemem.c` |
| `Cursor` | B-tree cursor handle | `VdbeCursor` |
| `Sorter` | External sort | `vdbesort.c` |

**Implementation:** `src/vdbe/`

**Estimated lines:** ~25,000

**Instruction format:**

```rust
struct Instruction {
    opcode: Opcode,    // Operation
    p1: i32,           // First operand (usually register)
    p2: i32,           // Second operand (usually jump target)
    p3: i32,           // Third operand
    p4: P4,            // Dynamic operand (string, blob, etc.)
    p5: u16,           // Flags
}
```

**Key opcodes (192 total):**

| Category | Examples |
|----------|----------|
| Control | `Goto`, `If`, `IfNot`, `Halt`, `Return` |
| Cursor | `OpenRead`, `OpenWrite`, `Close`, `Next`, `Prev`, `Seek*` |
| Column | `Column`, `Rowid`, `MakeRecord`, `Insert`, `Delete` |
| Arithmetic | `Add`, `Subtract`, `Multiply`, `Divide` |
| Comparison | `Eq`, `Ne`, `Lt`, `Le`, `Gt`, `Ge` |
| Aggregation | `AggStep`, `AggFinal`, `AggValue` |
| String | `Concat`, `Substr`, `Length` |
| Result | `ResultRow`, `Copy`, `Move` |

### Layer 5: B-Tree

Logical storage as balanced trees.

| Component | Responsibility | SQLite equivalent |
|-----------|----------------|-------------------|
| `BTree` | Tree operations (insert, delete, search) | `btree.c` |
| `BTreeCursor` | Positioned iteration | `BtCursor` |
| `Cell` | Key-value encoding | Cell format |
| `Page` | Page layout (interior vs leaf) | Page types |
| `Overflow` | Large value handling | Overflow pages |

**Implementation:** `src/btree/`

**Estimated lines:** ~10,000

**Page types:**

| Type | Header | Content |
|------|--------|---------|
| Interior index | 12 bytes | Child pointers + keys |
| Leaf index | 8 bytes | Keys only |
| Interior table | 12 bytes | Child pointers + rowids |
| Leaf table | 8 bytes | Rowids + payloads |

### Layer 6: Pager

Page cache and transaction journaling.

| Component | Responsibility | SQLite equivalent |
|-----------|----------------|-------------------|
| `Pager` | Page cache, dirty tracking | `pager.c` |
| `PageCache` | LRU buffer pool | `pcache.c` |
| `Journal` | Rollback journal | Journal file |
| `Wal` | Write-ahead log | `wal.c` |
| `Checkpoint` | WAL → main DB | Checkpoint modes |

**Implementation:** `src/pager/`

**Estimated lines:** ~12,000

**Journal modes:**

| Mode | Behavior |
|------|----------|
| `DELETE` | Delete journal after commit |
| `TRUNCATE` | Truncate journal to zero |
| `PERSIST` | Keep journal, invalidate header |
| `MEMORY` | Journal in RAM only |
| `WAL` | Write-ahead logging |
| `OFF` | No journal (dangerous) |

**Locking states:**

```
UNLOCKED → SHARED → RESERVED → PENDING → EXCLUSIVE
    ↑         ↓
    └─────────┘
```

### Layer 7: OS Interface (VFS)

Platform abstraction for I/O.

| Component | Responsibility | SQLite equivalent |
|-----------|----------------|-------------------|
| `Vfs` | Virtual filesystem trait | `sqlite3_vfs` |
| `File` | File handle trait | `sqlite3_file` |
| `UnixVfs` | POSIX implementation | `os_unix.c` |
| `WindowsVfs` | Windows implementation | `os_win.c` |
| `MemVfs` | In-memory filesystem | `:memory:` |

**Implementation:** `src/vfs/`

**Estimated lines:** ~8,000

**VFS trait:**

```rust
trait Vfs {
    fn open(&self, path: &Path, flags: OpenFlags) -> Result<Box<dyn File>>;
    fn delete(&self, path: &Path) -> Result<()>;
    fn exists(&self, path: &Path) -> Result<bool>;
    fn full_path(&self, path: &Path) -> Result<PathBuf>;
    fn random(&self, buf: &mut [u8]);
    fn current_time(&self) -> f64;
}

trait File {
    fn read(&self, buf: &mut [u8], offset: u64) -> Result<usize>;
    fn write(&self, buf: &[u8], offset: u64) -> Result<usize>;
    fn truncate(&self, size: u64) -> Result<()>;
    fn sync(&self, flags: SyncFlags) -> Result<()>;
    fn size(&self) -> Result<u64>;
    fn lock(&self, level: LockLevel) -> Result<()>;
    fn unlock(&self, level: LockLevel) -> Result<()>;
}
```

## File Format

sqlite-rs MUST read and write files byte-compatible with SQLite 3.x.

### Database Header (100 bytes)

| Offset | Size | Field |
|--------|------|-------|
| 0 | 16 | Magic: `SQLite format 3\0` |
| 16 | 2 | Page size (512–65536) |
| 18 | 1 | Write version (1=legacy, 2=WAL) |
| 19 | 1 | Read version |
| 20 | 1 | Reserved space per page |
| 24 | 4 | File change counter |
| 28 | 4 | Database size in pages |
| 32 | 4 | First freelist trunk page |
| 36 | 4 | Total freelist pages |
| 40 | 4 | Schema cookie |
| 44 | 4 | Schema format (1–4) |
| 48 | 4 | Default page cache size |
| 52 | 4 | Largest root btree page (auto-vacuum) |
| 56 | 4 | Text encoding (1=UTF-8, 2=UTF-16LE, 3=UTF-16BE) |
| 60 | 4 | User version |
| 64 | 4 | Incremental vacuum mode |
| 68 | 4 | Application ID |
| 72 | 20 | Reserved |
| 92 | 4 | Version-valid-for |
| 96 | 4 | SQLite version number |

### Page Layout

```
┌─────────────────────────────────────────┐
│  Page Header (8 or 12 bytes)            │
├─────────────────────────────────────────┤
│  Cell Pointer Array (2 bytes × N cells) │
├─────────────────────────────────────────┤
│  Unallocated Space                      │
├─────────────────────────────────────────┤
│  Cell Content Area (grows upward)       │
├─────────────────────────────────────────┤
│  Reserved Region (per header byte 20)   │
└─────────────────────────────────────────┘
```

## Implementation Scope Summary

| Layer | Lines (est.) | Difficulty | Rust advantage |
|-------|--------------|------------|----------------|
| SQL Interface | 2,000 | Low | Ergonomic API |
| Frontend | 8,000 | Medium | Parser combinators |
| Code Generator | 35,000 | High | Pattern matching |
| VDBE | 25,000 | High | Enum opcodes |
| B-Tree | 10,000 | Very High | Memory safety |
| Pager | 12,000 | Very High | Concurrency |
| VFS | 8,000 | Medium | Trait abstraction |
| **Total** | **~100,000** | | |

## Requirements

### Requirement 1: Layer Isolation [MUST]

Each layer MUST communicate only through its defined interface. No layer SHALL reach into another layer's internals.

**Implementation:** `src/lib.rs` (planned)

#### Scenario: B-tree does not know SQL

- GIVEN a B-tree cursor positioned on a row
- WHEN reading the row's content
- THEN the B-tree MUST return raw bytes, not parsed columns

#### Scenario: VDBE does not know file format

- GIVEN a VDBE program accessing a table
- WHEN executing an `OpenRead` instruction
- THEN the VDBE MUST call B-tree API, not pager API directly

### Requirement 2: File Format Compatibility [MUST]

sqlite-rs MUST read and write files that SQLite 3.x can read and write.

**Implementation:** `src/lib.rs` (planned)

**Tests:** `tests/corpus/harness.rs` (planned)

#### Scenario: Read SQLite file

- GIVEN a database created by SQLite 3.45
- WHEN sqlite-rs opens it
- THEN all tables, indices, and data MUST be accessible

#### Scenario: Write SQLite file

- GIVEN a database created by sqlite-rs
- WHEN SQLite 3.45 opens it
- THEN all tables, indices, and data MUST be accessible

### Requirement 3: Test Oracle [MUST]

Development MUST use SQLite's test suite as the compatibility oracle.

**Implementation:** `tests/corpus/oracle.rs`

**Tests:** `tests/corpus/harness.rs`

#### Scenario: Query result match

- GIVEN any SQL query Q and database D
- WHEN both SQLite and sqlite-rs execute Q on D
- THEN the results MUST be identical (byte-for-byte for BLOBs, value-equal otherwise)

**Tests:** `tests/parity/v01.rs::acceptance_and_output_match_across_readable_corpus`

### Requirement 4: Tier 0 Read-Completeness [MUST]

sqlite-rs MUST be able to extract every stored row from any well-formed SQLite database, regardless of which SQLite feature created it. Unsupported feature semantics MUST degrade to raw-row access, never to errors.

**Implementation:** `src/lib.rs` (planned)

**Tests:** `tests/tiers/tier0.rs::t0_feature_bearing_files_are_raw_row_readable`

#### Scenario: Read a WITHOUT ROWID table

- GIVEN a database containing a WITHOUT ROWID table (stored as an index b-tree)
- WHEN sqlite-rs dumps the database
- THEN all rows of that table MUST be produced, even if WITHOUT ROWID write semantics are unimplemented

**Tests:** `src/btree/index.rs::without_rowid_table_is_readable_as_index_btree`

#### Scenario: Read a database with uncheckpointed WAL

- GIVEN a WAL-mode database with a non-empty `-wal` file
- WHEN sqlite-rs reads the database
- THEN the page view MUST include committed WAL frames — the data MUST match what `sqlite3` reports

**Tests:** `src/pager.rs::tests::fixtures::wal_pending_fixture_shows_uncheckpointed_rows`

#### Scenario: Read a UTF-16 database

- GIVEN a database created with `PRAGMA encoding='UTF-16le'` (or UTF-16be)
- WHEN sqlite-rs dumps text values
- THEN text MUST be decoded correctly

#### Scenario: Unknown schema entry degrades gracefully

- GIVEN a database containing a virtual table (e.g. FTS5) whose module is unimplemented
- WHEN sqlite-rs dumps the database
- THEN the shadow tables' raw rows MUST be readable and no error raised for the unknown module

**Tests:** `src/schema/ddl_reader.rs::fts5_virtual_table_is_graceful_unknown_shadow_tables_are_readable`

#### Scenario: Hot journal is never ignored

- GIVEN a database with a hot rollback journal (crashed writer)
- WHEN sqlite-rs opens it read-only
- THEN it MUST NOT serve pre-rollback pages as committed data — it either applies recovery semantics or refuses with a clear error

**Tests:** `src/pager.rs::tests::fixtures::hot_journal_fixture_recovers_committed_state`

#### Scenario: Reader takes a SHARED lock before serving pages

- GIVEN a `Pager` opened over a database file
- WHEN it is open
- THEN it MUST hold a journal-mode SHARED byte-range lock (`PENDING_BYTE+2` / `SHARED_SIZE`) on the file, blocking a concurrent writer's EXCLUSIVE lock, and release it when dropped

**Tests:** `src/pager.rs::tests::open_acquires_shared_lock_released_on_drop`, `src/vfs/lock.rs::tests::shared_lock_blocks_concurrent_exclusive_lock_until_dropped`

#### Scenario: Lock contention is reported as busy, not a generic I/O error

- GIVEN a database file whose journal-mode SHARED-lock byte range is held EXCLUSIVE by another process
- WHEN sqlite-rs attempts to open the database for reading
- THEN it MUST surface a distinguishable "database is locked" error, not a generic I/O failure

**Tests:** `src/vfs/lock.rs::tests::lock_shared_fails_with_contention_errno_when_exclusively_held_elsewhere`

#### Scenario: WAL reader claims a reader-mark slot so a live checkpointer backs off

- GIVEN a WAL-mode database with an adjacent `-shm` file
- WHEN a `Pager` opens the database
- THEN it MUST claim a `WAL_READ_LOCK` slot and publish its `aReadMark` value, blocking a concurrent checkpointer from backfilling/truncating past that point, and release the slot when dropped

**Tests:** `src/pager.rs::tests::open_claims_wal_read_lock_when_shm_present_released_on_drop`, `src/vfs/shm.rs::tests::claims_a_slot_and_publishes_mx_frame`, `src/vfs/shm.rs::tests::contended_slot_is_skipped_for_the_next_free_one`
