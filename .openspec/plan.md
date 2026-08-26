# Development Plan

Value-driven breakdown for rebuilding SQLite in Rust. Each block delivers usable capability — go from working to working. Grammar, architecture layers, and test corpus are sliced per block, not built layer-by-layer.

## Core Definition & Drop Order

The project invariant: **whatever wrote the file, sqlite-rs must be able to read the data out.** Reading is much smaller than supporting — a `WITHOUT ROWID` table, a STRICT table, an FTS5 index are all just b-trees with records on disk. You don't need a feature's semantics to read its data. The storage layer must therefore be **100% format-complete before any SQL feature is scoped**.

### Tier 0 — READ CORE (never droppable)

| Capability | Why it's in the core |
|------------|---------------------|
| All serial types + varints | Every value ever stored: NULL, all int widths, both REAL encodings, text, blob |
| All text encodings | UTF-8, UTF-16LE, UTF-16BE databases exist in the wild |
| Table b-trees AND index b-trees | `WITHOUT ROWID` tables are stored as index b-trees — dropping index-btree reading silently drops whole tables |
| Overflow chains, all page sizes (512–65536), reserved bytes | Or arbitrary rows become unreadable |
| **WAL frame reading** | A db with an uncheckpointed `-wal` file is *incomplete without it* — read-only WAL recovery is a read feature, not a durability feature |
| Hot journal detection | At minimum: refuse-and-explain, never serve pre-rollback pages as truth |
| `sqlite_master` schema decode | Names/types/DDL text — even for features we don't execute |
| Freelist / pointer-map awareness | Skip correctly in auto-vacuum databases |
| Graceful unknowns | Unknown schema entries (virtual tables, future formats) degrade to raw-row access, never to errors |

**Acceptance gate:** take *any* database — FTS5, R-Tree, generated columns, STRICT, WAL-mode with pending frames — and `sqlite-rs dump` produces every stored row. This is the project's floor.

### Tier 1 — QUERY CORE

Single-table SELECT with WHERE/ORDER BY/LIMIT, core scalar functions, correct type affinity, the three built-in collations. The query *planner* is droppable — full scans are always correct, just slow.

### Tier 2 — WRITE CORE

INSERT/UPDATE/DELETE on ordinary rowid tables, basic constraints, rollback-journal transactions, output that passes `PRAGMA integrity_check` in stock SQLite. Permitted simplifications: always-full journal mode, conservative locking, no auto-vacuum on write.

### Tier 3 — Drop order (last dropped first)

1. Multi-table read (joins/aggregates) — high value, read-only, safe
2. WAL *writing* (WAL reading is Tier 0)
3. Foreign keys + triggers
4. Modern SQL (UPSERT / RETURNING / window functions)
5. PRAGMAs beyond introspection
6. ALTER TABLE, VACUUM
7. *Writing to* WITHOUT ROWID / STRICT tables (reading them is Tier 0)
8. First to drop: extension *semantics* (FTS5/R-Tree queries), sessions, ATTACH, hooks

**The asymmetry, explicit:** WAL, WITHOUT ROWID, STRICT, and STORED generated columns each appear twice — reading their data is Tier 0; executing their semantics is Tier 3. Tier 0 ≈ V1 (extended), Tier 1 ≈ V2, Tier 2 ≈ V3+V5-minimum; everything beyond is negotiable in the order above.

## Value Blocks

| Block | Version | Value delivered | One-liner |
|-------|---------|-----------------|-----------|
| **V1** | 0.1–0.5 | Read existing SQLite files | "Open any .sqlite file, extract the data" |
| **V2** | 0.6–0.9 | Single-table queries | "Query it with real SQL" |
| **V3** | 0.10–0.12 | Full CRUD | "It's a database now" |
| **V4** | 0.13 | Multi-table SQL | "Joins, subqueries, aggregates" |
| **V5 Slim** | 0.14 | Core transactions | "ACID with a journal" |
| **V6 Slim** | 0.15 / **1.0** | WAL & basic CTEs | "Readers don't block writers" |
| **V7** | 1.1+ | Polish & compatibility | "Everything else" |
| **V8** | — | Integrity & triggers | "The schema enforces itself" |
| **V9** | — | Modern SQL | "UPSERT, RETURNING, window functions" |
| **V10** | — | Storage forms & operations | "STRICT, WITHOUT ROWID, VACUUM, ATTACH" |
| **V11** | — | Virtual tables & JSON | "Extension foundation + json1" |
| **V12** | — | Search & spatial | "FTS5, R-Tree" |

---

## V1 — Read Existing SQLite Files

**Value:** Open any `.sqlite`/`.db` file created by SQLite and extract data. No SQL yet — a programmatic API. Immediately useful as a forensics/ETL library, and forces file-format correctness first (the highest-risk area). **V1 implements the full Tier 0 READ CORE** — feature-agnostic at the storage layer: WITHOUT ROWID tables, STRICT tables, extension shadow tables are all just b-trees here.

**Scope:**

| Layer | Subset |
|-------|--------|
| VFS | Read-only Unix VFS |
| Pager | Read path, hot-journal detection (refuse-and-explain), no cache eviction |
| WAL | **Read-only WAL recovery** — merge uncheckpointed `-wal` frames into the page view |
| B-Tree | Cursor read: first/next/seek, **table + index btrees** (index btrees carry WITHOUT ROWID tables), overflow pages |
| Record format | Serial types, varint decoding, all value types, **all three text encodings (UTF-8/16LE/16BE)** |
| Format edge cases | All page sizes (512–65536), reserved bytes, freelist + pointer-map (auto-vacuum) skipping |
| Schema | Parse `sqlite_master` (minimal DDL reader, not full parser); unknown entries degrade to raw-row access |

**Grammar:** none (CREATE TABLE text in `sqlite_master` parsed with a minimal DDL reader).

**Corpus:** fixture databases generated by `sqlite3`; hex-level format tests; every serial type; overflow chains; UTF-16 databases; WAL-mode databases with uncheckpointed frames; auto-vacuum databases; FTS5/R-Tree/STRICT/WITHOUT ROWID fixtures (raw-row readable).

**Demo:** `sqlite-rs dump file.db` — prints schema + all rows, byte-identical values vs `sqlite3 file.db .dump`, for *any* fixture including WAL-pending and extension-bearing databases.

---

## V2 — Single-Table Queries

**Value:** Real SQL against real files. `SELECT` with WHERE, ORDER BY, LIMIT on one table. This is where tokenizer, parser, codegen, and VDBE come alive — on the narrowest useful grammar slice.

**Scope:**

| Layer | Subset |
|-------|--------|
| Tokenizer | Complete (all ~140 keywords — build once, fully) |
| Parser | `SELECT` core: result columns, single FROM table, WHERE, ORDER BY, LIMIT/OFFSET |
| Expressions | Literals, column refs, unary/binary ops, `IS NULL`, `BETWEEN`, `IN (list)`, `LIKE`, `CASE`, `CAST` |
| Codegen | Full-table scan; index lookup for simple equality (stretch) |
| VDBE | 52 core opcodes (harvested, `tools/opcodes-v2.json`, #87): control, cursor read, Column, comparisons, arithmetic, ResultRow, sorter. DISTINCT's dedup path costs a second in-memory-ephemeral-table opcode family (`OpenEphemeral`/`Sequence`/`IdxInsert`/`Found`/`Delete`), not just a flag |
| Functions | Scalar core: `length`, `upper`, `lower`, `substr`, `abs`, `coalesce`, `typeof` |
| Type system | Affinity rules, comparison/collation semantics (BINARY, NOCASE, RTRIM) |

**Grammar slice:** ~40 of ~200 productions.

**Corpus:** sqllogictest `select1`-level slice; SQLite `select1.test`, `expr.test`, `like.test`; affinity edge cases (`affinity2.test`). Oracle-diff every query result against `sqlite3`.

**Demo:** `sqlite-rs query file.db "SELECT name, price FROM products WHERE price > 10 ORDER BY name LIMIT 5"` — identical output to sqlite3.

---

## V3 — Full CRUD

**Value:** Write path. INSERT/UPDATE/DELETE plus CREATE/DROP TABLE and CREATE/DROP INDEX. Files written must open cleanly in stock SQLite — that's the compatibility proof.

**Scope:**

| Layer | Subset |
|-------|--------|
| Pager | Write path, basic journaling for statement atomicity, locking states |
| B-Tree | Insert, delete, page split/merge/balance, freelist management |
| Parser | INSERT (VALUES + SELECT), UPDATE, DELETE, CREATE/DROP TABLE, CREATE/DROP INDEX |
| Codegen | Write opcodes, constraint checks (NOT NULL, UNIQUE, PK, CHECK, DEFAULT) |
| VDBE | MakeRecord, Insert, Delete, IdxInsert, NewRowid, auto-index maintenance |
| Schema | Schema cookie, sqlite_master maintenance, AUTOINCREMENT |

**Grammar slice:** +~50 productions (DML + core DDL, column/table constraints).

**Corpus:** `insert.test`, `update.test`, `delete.test`, `index.test`, `conflict.test` (basic ON CONFLICT), `autoinc.test`. Cross-validation: every file sqlite-rs writes is opened and `PRAGMA integrity_check`-ed by stock sqlite3.

**Demo:** create a database with sqlite-rs, insert 10K rows, open it in sqlite3 — everything reads perfectly. And vice versa.

---

## V4 — Multi-Table SQL

**Value:** The relational model. Joins, subqueries, aggregates, compound selects, non-recursive CTEs. This is the bulk of the code generator (~35K lines in C) and the block where the query planner earns its keep.

**Scope (slimmed — see Deferred below):**

| Layer | Subset |
|-------|--------|
| Parser | JOIN grammar (INNER, LEFT, CROSS), subqueries (scalar, IN, EXISTS), GROUP BY/HAVING, UNION/UNION ALL, non-recursive WITH/CTE |
| Planner | Join ordering, index selection, WHERE-clause analysis |
| Codegen | Nested-loop joins, coroutines/materialization for subqueries, aggregate compilation |
| VDBE | AggStep/AggFinal, OpenEphemeral, sorter |
| Functions | Aggregates: `count`, `sum`, `avg`, `min`, `max` |

**Grammar slice:** +~50 productions (slimmed from ~70).

**Deferred to V5+:**

| Feature | Target | Rationale |
|---------|--------|-----------|
| Recursive CTEs | V6 | Complex, rarely used in basic apps |
| Views (CREATE VIEW) | V6 | Syntactic sugar over subqueries |
| INTERSECT / EXCEPT | V6 | UNION covers 90% of use cases |
| group_concat | V6 | Exotic aggregate |
| Advanced join reordering | Perf epic #111 | Optimization, not correctness |

**Corpus:** `join*.test`, `select2-8.test`, `subquery*.test`, `with*.test` (non-recursive slice), `aggnested.test`; the bulk of sqllogictest (its 7.2M cases are mostly multi-table SELECTs — this block unlocks running the full set).

**Demo:** run the sqllogictest suite; report pass percentage as the public progress metric.

---

## V5 Slim — Core Transactions (0.14.0)

**Value:** ACID in the classic journal mode. BEGIN/COMMIT/ROLLBACK, hot-journal crash recovery. After this block a power cut cannot corrupt a database.

**Scope (Slim):**

| Layer | Subset |
|-------|--------|
| Parser | BEGIN/COMMIT/ROLLBACK, DEFERRED/IMMEDIATE/EXCLUSIVE |
| Pager | Journal mode DELETE, hot-journal recovery, all 5 lock states |
| VDBE | Transaction opcodes |

**Deferred to V7:**

| Feature | Rationale |
|---------|-----------|
| SAVEPOINT/RELEASE | Nested transactions — power user feature |
| Statement journals | Partial rollback — edge case |
| TRUNCATE/PERSIST/MEMORY journal | Alternative modes — DELETE is default |

**Grammar slice:** +~5 productions.

**Corpus:** `trans*.test`, `journal*.test`; crash simulation (kill -9 mid-commit, verify recovery).

**Demo:** power-cut torture test — loop of writes with random kill; database always recovers consistent, verified by stock sqlite3 `integrity_check`.

**Estimate:** 2-3 weeks.

---

## V6 Slim — WAL & Basic CTEs (0.15.0 / 1.0 candidate)

**Value:** Modern SQLite's default deployment mode. Readers don't block writers; writers don't block readers. Interoperates with stock SQLite processes on the same database file. Plus basic relational completeness.

**Scope (Slim):**

| Layer | Subset |
|-------|--------|
| WAL | WAL file format, SHM index (wal-index), reader marks |
| Checkpoint | PASSIVE mode |
| Concurrency | Multi-reader single-writer |
| Pager | `journal_mode=WAL` switching in both directions |
| Parser | Non-recursive WITH/CTE, UNION (dedup), CREATE VIEW |
| Codegen | CTE materialization, sorter for UNION dedup |

**Deferred to V7:**

| Feature | Rationale |
|---------|-----------|
| Recursive CTEs | Complex fixpoint evaluation |
| FULL/RESTART/TRUNCATE checkpoint | PASSIVE sufficient for most apps |
| Auto-checkpoint | Manual checkpoint works |
| busy_handler/busy_timeout | Concurrency tuning, not core |
| group_concat | Convenience aggregate |
| INTERSECT/EXCEPT | Rare set operations |

**Grammar slice:** +~20 productions (CTEs, views, UNION).

**Corpus:** `wal*.test`, `with*.test` (non-recursive), `view.test`; interop: sqlite3 and sqlite-rs alternating reads/writes on the same WAL-mode database.

**Demo:** sqlite-rs writing while stock `sqlite3` reads the same file live — and the reverse.

**Estimate:** 3-4 weeks.

**1.0 Line:** After V6 Slim, sqlite-rs is feature-complete for most applications: full read/write path, JOINs, subqueries, indexes, ACID transactions, WAL concurrency, CTEs, views.

---

## V7 — Polish & Compatibility (0.17.0+)

**Value:** Completeness. Everything deferred from V5/V6 plus operability features. After V7, sqlite-rs handles the long tail of SQLite compatibility.

**Deferred from V5 (Transactions):**

| Feature | Description |
|---------|-------------|
| SAVEPOINT/RELEASE | Nested transactions |
| Statement journals | Partial rollback of failed statements |
| TRUNCATE/PERSIST/MEMORY journal | Alternative journal modes |

**Deferred from V6 (WAL/Relational):**

| Feature | Description |
|---------|-------------|
| Recursive CTEs | WITH RECURSIVE — fixpoint evaluation |
| FULL/RESTART/TRUNCATE checkpoint | Advanced checkpoint modes |
| Auto-checkpoint | Automatic WAL checkpoint |
| busy_handler/busy_timeout | Concurrency tuning |
| group_concat | Aggregate function |
| INTERSECT/EXCEPT | Set operations |

**PRAGMAs (priority order):**

| Tier | PRAGMAs |
|------|---------|
| Must | `table_info`, `table_list`, `index_list`, `index_info`, `database_list`, `schema_version`, `user_version`, `page_size`, `page_count`, `journal_mode`, `foreign_keys`, `integrity_check`, `quick_check` |
| Should | `cache_size`, `synchronous`, `auto_vacuum`, `encoding`, `application_id`, `busy_timeout`, `wal_checkpoint`, `optimize` |
| May | `compile_options`, `stats`, `function_list`, `collation_list`, `pragma_list` |

Plus: `EXPLAIN` / `EXPLAIN QUERY PLAN` (bytecode listing — nearly free since the VDBE is real bytecode) and `ANALYZE` (sqlite_stat1 for the planner).

**Corpus:** `pragma*.test`, `analyze*.test`, `savepoint*.test`, `walthread*.test`.

**Demo:** point an existing tool (e.g. `sqlite-utils`, Datasette, or an ORM's introspection) at sqlite-rs and have it work.

---

## V8 — Integrity & Triggers

**Value:** The schema enforces itself. Referential integrity, reactive logic, and schema evolution — the features that make SQLite safe under application churn.

**Scope:**

| Feature | Notes |
|---------|-------|
| Foreign keys | Enforcement, ON DELETE/UPDATE actions, cascades, deferred checking (`fkey*.test`) |
| Triggers | BEFORE/AFTER/INSTEAD OF, RAISE, recursive triggers (`trigger*.test`) |
| ALTER TABLE | RENAME, ADD/DROP/RENAME COLUMN incl. schema rewrite (`alter*.test`) |
| CHECK constraints | Full expression checks (already partial in V3, completed here) |

**Grammar slice:** +~15 productions (trigger bodies dominate).

**Corpus:** `fkey*.test`, `trigger*.test`, `alter*.test`.

**Demo:** a cascading delete through a 3-table FK chain with audit triggers, matching sqlite3 row-for-row.

---

## V9 — Modern SQL

**Value:** The SQL that post-2018 applications actually write. ORMs (Django 4+, Prisma, Diesel) emit these constructs by default.

**Scope:**

| Feature | Notes |
|---------|-------|
| UPSERT | `ON CONFLICT DO UPDATE/NOTHING` (`upsert*.test`) |
| RETURNING | On INSERT/UPDATE/DELETE (`returning*.test`) |
| Window functions | OVER, PARTITION BY, frame specs, named windows (`window*.test`) — grammar-heavy |
| Advanced aggregates | FILTER clause, DISTINCT in aggregates |
| Date/time functions | `date`, `time`, `datetime`, `julianday`, `strftime`, `unixepoch` |
| `IIF`, `NULLIF`, math functions | Scalar completeness |

**Grammar slice:** +~20 productions (window grammar dominates).

**Corpus:** `upsert*.test`, `returning*.test`, `window*.test`, `date.test`, `func*.test`.

**Demo:** run an unmodified Django/Prisma-generated query workload against sqlite-rs.

---

## V10 — Storage Forms & Operations

**Value:** Alternate table forms and database-level operations — completeness for schema designers and DBAs.

**Scope:**

| Feature | Notes |
|---------|-------|
| WITHOUT ROWID | Clustered-PK tables (`withoutrowid*.test`) |
| STRICT tables | Type enforcement (`strict*.test`) |
| Generated columns | VIRTUAL/STORED (`gencol*.test`) |
| ATTACH/DETACH | Multi-database connections, cross-DB queries (`attach*.test`) |
| VACUUM | Full rebuild + incremental, `VACUUM INTO` (`vacuum*.test`) |
| Auto-vacuum | Pointer-map pages |
| Collations | Custom collation registration API |
| Authorizer / hooks | Update/commit hooks, progress handler |

**Grammar slice:** +~15 productions → grammar complete (~200/200).

**Corpus:** `withoutrowid*.test`, `strict*.test`, `gencol*.test`, `attach*.test`, `vacuum*.test`.

**Demo:** `VACUUM INTO` produces a file byte-compatible with stock SQLite's output structure.

---

## V11 — Virtual Tables & JSON

**Value:** The extension foundation plus the single most-used extension. The virtual-table mechanism is the plugin architecture (per [sqlite.org/docs.html](https://www.sqlite.org/docs.html)); json1 is ubiquitous in modern apps.

**Scope:**

| Feature | Notes |
|---------|-------|
| Virtual table trait | `xCreate/xConnect/xBestIndex/xFilter/...` as a safe Rust trait |
| Table-valued functions | `generate_series`, `pragma_*` TVFs |
| json1 / JSONB | `json_extract`, `json_set`, `->`/`->>` operators, `json_each`, `json_tree`, JSONB binary format |
| `dbstat`, `csv` | Simple vtab exercises |

**Grammar:** `->`/`->>` operators, `CREATE VIRTUAL TABLE`.

**Corpus:** `json1*.test`, `tabfunc*.test`, `bestindex*.test`.

**Demo:** third-party Rust crate implements a custom vtab against the public trait — no unsafe code.

---

## V12 — Search & Spatial

**Value:** Ecosystem parity on the heavyweight extensions — the features that make SQLite a search engine and a GIS store.

**Scope:**

| Feature | Notes |
|---------|-------|
| FTS5 | Tokenizers, MATCH queries, ranking (bm25), highlight/snippet, shadow-table format compatibility |
| R-Tree | Spatial index, shadow-table format compatibility |
| Sessions / changesets | Sync use cases (stretch) |

**Corpus:** `fts5*.test`, `rtree*.test`.

**Demo:** open an existing FTS5-indexed database created by stock SQLite and run MATCH queries against it unchanged.

---

## Dependency Graph

```
V1 (read files)
 └─→ V2 (single-table SELECT)
      └─→ V3 (CRUD)
           ├─→ V4 (multi-table SQL) ─→ V7 (ANALYZE/EXPLAIN parts)
           └─→ V5 (journal transactions) ─→ V6 (WAL & concurrency)
V7 core pragmas: after V3
V8 (integrity/triggers): after V4 (+V5 for deferred FK)
V9 (modern SQL): after V4 (windows need the sorter/aggregate machinery)
V10 (storage forms): after V3; ATTACH after V5
V11 (vtab + JSON): after V4 (vtabs plug into FROM clause)
V12 (FTS/R-Tree): after V11 (built on vtab mechanism)
```

Parallelizable pairs: V4 ∥ V5 (planner track vs pager track), V8 ∥ V9 ∥ V10 (independent features), V11 → V12 sequential.

## Grammar Coverage per Block

| Block | Productions (cum.) | of ~200 |
|-------|--------------------|---------|
| V1 | 0 (minimal DDL reader) | 0% |
| V2 | ~40 | 20% |
| V3 | ~90 | 45% |
| V4 | ~130 | 65% |
| V5 | ~140 | 70% |
| V6 | ~200 | 100% (core) |
| V7 | ~200 | 100% (core) |
| V8 | ~190 | 95% |
| V9 | ~210* | — |
| V10 | complete | 100% |

*Window grammar pushes past the core-200 count; V11 adds `CREATE VIRTUAL TABLE` and JSON operators on top.

## Corpus Strategy per Block

- **V1:** self-made fixtures via `sqlite3`; hex-diff format tests
- **V2:** sqllogictest single-table slice + `select1/expr/like` TCL tests
- **V3:** write-path TCL tests + interop check (`integrity_check` by stock sqlite3)
- **V4:** **full sqllogictest run** — pass-rate becomes the project's public metric
- **V5:** crash/torture tests
- **V6:** WAL interop with live stock-sqlite3 processes
- **V7–V12:** per-feature TCL test files as acceptance gates

## Test Strategy

SQLite as oracle throughout — **at a pinned version and build**:

```bash
diff <(sqlite3 test.db "$SQL") <(sqlite-rs query test.db "$SQL")
```

**Pinned oracle (spike 002 finding, #6; isolated by spike #22):** macOS's system `sqlite3` (3.51.0) is compiled with `CODEC=see-cccrypt` and reserves 12 bytes/page even unencrypted. #22 broke the version/codec confound by compiling the same version (3.51.0) without the codec flag — it produced `reserved_space=0`, confirming the codec flag, not the version, is the cause. Fixture generation and oracle diffs MUST use a pinned, non-codec build (brew or compiled amalgamation, exact version recorded in the corpus harness) — sufficient regardless of version. Both reserved-byte cases (0 and 12) are kept as explicit fixtures — the codec accident is free edge-case coverage. Oracle drift across versions is real; version bumps are deliberate, reviewed events.

Every block's exit criterion: its corpus slice passes with zero diffs, and files written by sqlite-rs pass `PRAGMA integrity_check` in stock SQLite.

## Assurance Stack

Shift-left principle (from MVL): catch each defect class at the earliest phase that can catch it. Full inventory lives in spec 005-assurance (#25) — **the living document every assurance-touching ticket must keep current**. Summary:

| Phase | In place | Phase 1 of V1 adds (#26) | Deferred (deliberate) |
|-------|----------|--------------------------|------------------------|
| **Compile time** | rustc, clippy `-D warnings`, rustfmt, mvl-limit (#23), `#![forbid(unsafe_code)]` crate-wide (#66) | panic-surface lints, `cargo mvl total` experiment on `src/record/` | — |
| **Test time** | cargo test, llvm-cov (CI-gated at 80% line coverage, reported on every PR — #20), traceability dashboard | proptest roundtrips, cargo-fuzz on `decode_record` (discharges 003 Req 6) | **Mutation testing (cargo-mutants) → V1 exit gate** (epic #5): coverage proves execution, mutation score proves assertion |
| **Build time** | Cargo.lock, pinned oracle (004 Req 1) | `--locked` CI, cargo-deny (install at zero deps), SHA-pinned actions | **SBOM / cargo-auditable → publish time** |
| **Run time** | Structured error taxonomy | The Tier 0 totality claim: any input → `Ok` or structured `Err`, never panic (enforced at compile+fuzz time); `debug_assert!` invariants as code grows | **`integrity_check`-style self-diagnosis → V7** |

Deferred ≠ dropped: each deferred item has a named landing point, and moving that point is a plan change, not an omission.

## CLI & Tooling

The `sqlite3` CLI shell is a **separate program** from the library (`shell.c`, ~30K lines — vs ~150K for the library). It is where `-csv`/`-json` output modes, `.dump`, `.schema`, `.tables`, `.import`, `.mode`/`.headers`, and the REPL live. It is a third compatibility surface next to the file format and the SQL dialect — and it is also our test oracle's interface (`sqlite3 -csv`, `.dump`). Three levels, scoped explicitly:

| Level | Scope | Status |
|-------|-------|--------|
| **1. Dev tooling** | `sqlite-rs dump` / `query` / `export` — our own UX, no shell-compatibility claim. First piece: spike 005 (#12, CSV export) | **In scope** — grows with V1/V2 |
| **2. Output-format parity** | Match the shell's `-csv`, `-json`, `-list` output modes exactly, including NULL representation, quoting, blob and float rendering | **In scope** — folded into the V1 step 9 output contract; makes oracle diffing trivial forever |
| **3. Shell parity** | `.dump`, `.schema`, `.tables`, `.import`, REPL, the full dot-command set | **Explicit non-goal for now** — candidate for a later V13 "CLI shell" block; `.dump` (portable backup) and `.schema` are the first candidates when demand appears |

Rationale for level 2 being early: every oracle test diffs our output against the shell's. If our formatting matches the shell's by construction, corpus diffs need zero normalization layers — the formatting *is* part of the evidence chain.

## Concurrency Contract

The compatibility contract has **two halves**: the file format (static — what the bytes mean) and the **locking protocol** (dynamic — how live processes coordinate). SQLite has no server; the OS's advisory file locking IS the concurrency infrastructure. A stock `sqlite3` process and sqlite-rs *will* open the same file simultaneously — that is the real deployment (Rust app + sqlite3 CLI for debugging). Format-right but locking-wrong yields the worst failure mode: silent corruption visible only under concurrent access.

**The protocol to reproduce:**

| Mechanism | Detail |
|-----------|--------|
| Journal-mode locks | 5 states (UNLOCKED → SHARED → RESERVED → PENDING → EXCLUSIVE) via POSIX `fcntl` range locks on the reserved lock-byte range (1073741824–1073742335: `PENDING_BYTE`=1073741824, `RESERVED_BYTE`=+1, `SHARED_FIRST`=+2, `SHARED_SIZE`=510) |
| WAL-mode locks | 8 lock bytes at `-shm` offset 120–127 (WRITE=120, CKPT=121, RECOVER=122, READ(0..4)=123–127, DMS=128); readers claim a `WAL_READ_LOCK(k)` slot and set `aReadMark[k]` (at `-shm` offset 100+4k) to the frame they need, so checkpointers don't overwrite frames in use |
| POSIX close() trap | `close()` on ANY fd of a file drops ALL the process's locks — must replicate SQLite's per-inode fd cache workaround |
| Threading | One connection per thread (serialized mode default within a connection) |
| Known limits | Advisory only (non-cooperating writers can corrupt); unreliable on network filesystems (NFS = #1 real-world corruption cause); WAL requires same-machine (mmap) |

**Tiered through the blocks:**

| Tier / Block | Concurrency obligation |
|--------------|------------------------|
| **Tier 0 / V1** | *Safe reader:* take SHARED correctly so live stock-SQLite writers see us (and we never read torn pages). Hot-journal and busy detection. **Validated on macOS** by spike 005 (#8) |
| **V3 (write core)** | Full journal-mode lock ladder incl. RESERVED/PENDING semantics and the fd-cache workaround. Lock ladder + PENDING anti-starvation **validated on macOS** by spike 005 (#8); the fd-cache itself still needs a real per-inode implementation — spike only reproduced the close() trap and confirmed the workaround's *shape* |
| **V5 (transactions)** | Busy handler, `busy_timeout`, deadlock-avoiding lock upgrade rules |
| **V6 (WAL)** | Exact `-shm` layout and lock-slot protocol; live interop with stock sqlite3 readers/writers/checkpointers as acceptance test. Reader-mark protocol (`aReadMark` + `WAL_READ_LOCK(k)`) **validated on macOS** by spike 005 (#8): a live sqlite3 checkpointer correctly backed off while our raw-fcntl claim was held, and proceeded once released |

**De-risking:** spike 005 (#8, `tests/spike/005_locking_interop` — numbered 005 on disk since `004` was already taken by a concurrent spike; this section's "spike 004" references predate that renumbering) validated the fcntl lock-byte offsets, PENDING anti-starvation semantics, the WAL read-lock/reader-mark protocol, and the close()-drops-all-locks trap (confirming the fix must be a real per-inode fd-cache — `dup()` does **not** work, since POSIX fcntl record locks are scoped to (process, inode), not the open file description) against a live stock `sqlite3` process. **macOS only** — Linux exercise is tracked separately (#42); see `tests/spike/005_locking_interop/findings.md` for full results and the one methodology finding (one-shot `sqlite3` CLI invocations auto-checkpoint on close, independent of another process's lock — a harness confound to watch for in #21's fixture work too).

## Dependencies

Aspirational list from early planning — stale; see `DEPENDENCIES.md` for the
actual current dependency set, and [ADR-0030](adr/0030-zero-proc-macro-dependencies.md)
for why `thiserror` was dropped in favor of hand-rolled error impls.

```toml
[dependencies]
lemon-rs = "0.x"        # Parser generator (or lalrpop)
memmap2 = "0.x"         # Memory-mapped I/O for VFS
parking_lot = "0.x"     # Faster mutexes for pager

[dev-dependencies]
rusqlite = "0.x"        # Oracle for testing
tempfile = "3"          # Test fixtures
```

## Risk Areas

| Area | Risk | Mitigation |
|------|------|------------|
| **File format** | Must be byte-compatible | V1 first — format risk retired earliest |
| **B-Tree balancing** | Complex, many edge cases | V3 write-path tests + fuzzing against oracle |
| **Crash recovery** | Subtle, catastrophic if wrong | V5 torture tests, SQLite crash-test patterns |
| **WAL/SHM interop** | Cross-process shared memory with stock SQLite | V6 live-interop tests, exact wal-index format |
| **Codegen size** | ~35K lines equivalent | Sliced across V2/V3/V4 — never a big-bang |
| **Type affinity** | Weird, underdocumented behavior | Oracle-diff in V2, `affinity*.test` early |
| **Shadow-table formats** | FTS5/R-Tree on-disk compat | V12 opens stock-created FTS databases as acceptance test |
