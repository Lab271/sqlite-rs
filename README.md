# sqlite-rs

A binary compatible Rust replication of SQLite.

## Why

SQLite is the most widely deployed database in the world — every phone, browser, and OS ships it, with over a trillion active deployments. Its dominance rests on four pillars:

- **Reliability:** 100% MC/DC test coverage, aviation-grade (it literally flies in the A350)
- **Zero ops:** no server, no config, one file
- **Stability promise:** file format frozen until 2050, public domain
- **Ubiquity:** the de-facto standard for embedded, transactional, SQL storage

The file format and SQL dialect are the moat. That is why sqlite-rs targets *compatibility* rather than a new engine: the interesting gap in the market is not "a better SQLite" — it is a **memory-safe, extensible SQLite**.

### The safety thesis

SQLite's legendary test discipline (a 700:1 test-to-code ratio, 100% MC/DC) exists because in C, the test suite must prove two things at once: *the code doesn't corrupt memory* and *the code computes the right answer*. Every recent SQLite CVE is a memory-safety failure downstream of an arithmetic error — an integer overflow or truncation feeding an unchecked read or write.

sqlite-rs splits that burden: **the Rust compiler carries the memory-safety half by construction** (`forbid(unsafe_code)`, ownership instead of `Mem`'s lifetime flags, enums instead of flag-tagged unions, checked arithmetic instead of silent truncation), so the entire evidence budget — the pinned-oracle corpus, fuzzing, property tests, mutation runs — is spent on the only question types cannot answer: *is the behavior SQLite's behavior?*

The honest caveat: safety is not correctness. Memory-safe code can still return the wrong answer politely — which is why every claim in this repo is ultimately backed by a byte-level diff against a pinned stock `sqlite3`, not by the type system alone.

## The Landscape

SQLite has no credible replacement in its core niche — embedded, transactional, SQL, zero-config. The alternatives all occupy adjacent niches:

### Embedded relational (SQLite's home turf)

| DB | Angle | Trade-off |
|----|-------|-----------|
| **DuckDB** | The real challenger — "SQLite for analytics" (OLAP, columnar) | Not for transactional workloads |
| **libSQL / Turso** | SQLite fork + replication, server mode | Still C SQLite at heart |
| **limbo (Turso)** | SQLite-compatible pure Rust rewrite | Early stage — same bet as this project |
| **Firebird Embedded** | Full-featured, stored procedures | Tiny mindshare |
| **H2 / Derby** | JVM world | Java-only niche |

### Embedded key-value (when you don't need SQL)

| DB | Angle |
|----|-------|
| **RocksDB** | LSM-tree, write-heavy, powers many databases internally |
| **LMDB** | Memory-mapped B-tree, read-blazing, crash-proof |
| **redb / sled** | Rust-native options — redb is the serious one |
| **FoundationDB** | Distributed KV, transactional |

### Client-server (when you outgrow single-node)

- **PostgreSQL** — the default answer for almost everything serious
- **MySQL / MariaDB** — legacy web scale
- **CockroachDB / TiDB** — distributed Postgres/MySQL-compatible

DuckDB is the only genuinely new force in the embedded space, and it deliberately took the *other* half (analytics) rather than compete head-on. The two are complements: applications increasingly ship both.

Meanwhile the "SQLite in production" renaissance (Litestream, LiteFS, Rails 8 defaults, fly.io) has made single-node SQLite fashionable again for server-side workloads — raising the value of a memory-safe implementation.

## Goals

- **Drop-in file compatibility** — read and write `.sqlite` files byte-compatible with SQLite 3.x
- **Dialect compatibility** — accept what SQLite accepts, reject what it rejects, using SQLite's own test corpus as the oracle
- **Memory safety** — Rust's ownership model across the B-tree, pager, and WAL (the durability-critical path)
- **Safe extensibility** — a virtual-table trait that lets third parties extend the engine without `unsafe`

## Plan

Development proceeds in twelve value blocks — each delivers usable capability, going from working to working. From reading existing SQLite files (V1), through single-table queries, CRUD, multi-table SQL, transactions and WAL, up to virtual tables, JSON, and FTS5 (V12).

See [.openspec/plan.md](.openspec/plan.md) for the full breakdown and [.openspec/](/.openspec) for architecture and parser specifications.

## Status

**Version 0.18.2** — see [CHANGELOG.md](CHANGELOG.md). One minor version per completed plan phase; V1 = 0.1.0–0.4.0, V2 = 0.5.0–0.8.0, V3 = 0.9.0–0.12.0.

All four V1 phases landed: format core (header, record decoder, VFS, pinned-oracle corpus), b-tree cursors incl. WITHOUT ROWID + minimal DDL reader, mid-life reading (pager, WAL frame recovery, safe-reader locking validated against live sqlite3), and the `sqlite-rs dump`/`export` CLI with shell-parity output.

All four V2 phases landed: tokenizer + SELECT-core parser, the value-semantics kernel (affinity, comparison, collation) and scalar function core, the VDBE interpreter with full control/cursor/compare/arithmetic/result/sorter opcode dispatch, and the `sqlite-rs query` CLI validated against a sqllogictest single-table slice. Epic #56 closed at 0.8.0.

V3 phase 1 (the b-tree write path) landed at 0.9.0: pager write path + freelist, table and index b-tree insert/delete with page split/merge/collapse, overflow chain write/free, and statement-level rollback journaling. Every file this crate writes opens and `PRAGMA integrity_check`s cleanly in stock `sqlite3`. Next: V3 phase 2 — the write-path parser + schema layer.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build/test instructions, code style, and the PR process. Security issues should go through [SECURITY.md](SECURITY.md) rather than a public issue.
