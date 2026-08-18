# SQLite Landscape: Rust Ecosystem

**Updated:** 2026-08-18

## Three Categories

The "Rust + SQLite" space divides into three distinct categories:

1. **Clean-room SQLite-compatible rewrites** — byte-compatible file format + SQL dialect
2. **Rust-native embedded databases** — compete with SQLite but own format/dialect
3. **Rust interfaces to actual SQLite** — bindings to the C library

sqlite-rs is in category 1, alongside Turso.

---

## Category 1: SQLite-Compatible Rewrites

| | **Turso Database** | **sqlite-rs** |
|---|---|---|
| **Origin** | Turso (ChiselStrike), evolved from libSQL fork | Independent research project |
| **Repo** | tursodatabase/turso (formerly limbo) | iheitlager/sqlite-rs |
| **Thesis** | Performance: concurrent writes + async I/O | Safety: memory-safe core for certification |
| **Key features** | MVCC (`BEGIN CONCURRENT`), io_uring, vector search | `forbid(unsafe_code)`, CVE-proof read path, audit trail |
| **Target market** | Cloud edge, high-concurrency workloads | Safety-critical embedded, forensics, certification |
| **Compatibility** | File format + wire + C API | File format + SQL dialect (no C API goal) |
| **Architecture** | Async-first, MVCC replaces WAL locking | Same architecture, safe implementation |
| **Maturity** | Beta (Jul 2026), libSQL remains production path | V2 complete (Aug 2026), read + single-table query |

## Turso Background

- **libSQL:** Turso's original project — a SQLite fork with 12k+ GitHub stars, native replication, vector search. Powers Turso cloud platform. Production-ready.
- **Limbo (Dec 2024):** Started as an experiment in Rust rewrite. Worked well enough to become the main direction.
- **Rename (2026):** Limbo → Turso Database. The Rust engine is now the intended successor to libSQL.
- **Postgres frontend (Jul 2026):** Turso announced effort to build Postgres compatibility on the same VM, betting the bytecode architecture can support multiple frontends.

## Positioning Analysis

### Why two rewrites can coexist

**Different theses, different markets:**

1. **Turso** — "SQLite but faster under concurrent load"
   - Customers: cloud developers, edge computing, high-write workloads
   - Pain point solved: SQLite's single-writer limitation
   - Value prop: concurrent writes without giving up SQLite's simplicity

2. **sqlite-rs** — "SQLite but memory-safe for certification"
   - Customers: aviation, medical devices, rail, forensics
   - Pain point solved: 70% of SQLite CVEs are memory safety
   - Value prop: compiler-verified safety, certification evidence

### Competitive dynamics

**Validation:** Turso raised money and has a team. The market believes Rust SQLite is viable.

**No collision (yet):** Performance vs safety are orthogonal theses. Turso's customers don't need DO-178C compliance; sqlite-rs's customers don't need `BEGIN CONCURRENT`.

**Risk scenario:** If Turso achieves MVCC + async + `forbid(unsafe_code)`, they could claim both performance AND safety. But:
- MVCC requires complex concurrency primitives — hard to keep safe
- Turso's async architecture likely requires unsafe for io_uring integration
- Certification requires audit trails, not just safety — that's sqlite-rs's explicit focus

**Opportunity:** Be "the boring safe one." While Turso chases features, sqlite-rs can be the implementation regulators trust.

---

## Category 2: Rust-Native Embedded Databases

These compete with SQLite's niche but don't aim for compatibility:

| Project | Type | Notes |
|---------|------|-------|
| **redb** | Key-value, ACID | "LMDB in Rust" — pure Rust, embedded, transactions |
| **sled** | Key-value | Older, less actively developed |
| **GlueSQL** | SQL engine | Pluggable backends, runs in-browser |
| **native_db** | Typed storage | Rust-native typed embedded storage |
| **fjall** | LSM-based | Log-structured merge tree |
| **LanceDB** | Vector DB | AI workloads, not relational |
| **Stoolap** | SQL, MVCC | SQLite-*like* not compatible, Volcano architecture (0.2 Jan 2026) |
| **SQLRite** | SQL + Vector | Learning project, MVCC, HNSW indexing, multi-language SDKs |

**Note:** Stoolap and SQLRite are SQLite-*like* (embedded SQL, single file) but not file-format compatible.

---

## Category 3: Rust Interfaces to SQLite

Mature ecosystem for using real SQLite from Rust:

| Crate | Type | Notes |
|-------|------|-------|
| **rusqlite** | Lightweight wrapper | The standard choice |
| **SQLx** | Async SQL toolkit | Compile-time checked queries |
| **Diesel** | ORM | Compile-time SQL checking |
| **SeaORM** | Async ORM | Dynamic, async-first |
| **turso crate** | Turso client | Near drop-in for rusqlite |

---

## Not Rust (Common Confusion)

| Project | Language | Notes |
|---------|----------|-------|
| **libSQL** | C | Turso's production fork of SQLite |
| **DuckDB** | C++ | Analytics, different niche |
| **rqlite** | Go | Distributed SQLite |

---

## sqlite-rs Position

**Category:** 1 (clean-room compatible rewrite)

**Unique angle:** The only rewrite focused on *certification and safety* rather than *performance and features*.

| Need | → Use |
|------|-------|
| SQLite file compatibility today | rusqlite |
| SQLite compatibility + concurrent writes (when stable) | Turso |
| SQLite compatibility + certification evidence | sqlite-rs |
| Small embedded ACID in pure Rust (no SQL compat needed) | redb |

---

## Links

- Turso: https://turso.tech/
- Turso Database repo: https://github.com/tursodatabase/turso
- libSQL: https://github.com/tursodatabase/libsql
- sqlite-rs: https://github.com/iheitlager/sqlite-rs
- redb: https://github.com/cberner/redb
- GlueSQL: https://github.com/gluesql/gluesql

## Related

- [cve-assessment.md](cve-assessment.md) — why memory safety matters (the CVE evidence)
- [README.md](../README.md) — sqlite-rs positioning and goals
