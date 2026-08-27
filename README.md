# sqlite-rs

A safe binary compatible Rust replication of SQLite without any reliance on external libraries.

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

### Zero external dependencies

sqlite-rs targets security-sensitive contexts where every dependency is a trust boundary and proc macros are the worst case — they execute arbitrary code at build time, not just at run time. The production build has **zero external dependencies**: proc-macro-based error enums and the CLI's line editor were replaced with hand-rolled equivalents ([ADR-0030](.openspec/adr/0030-zero-proc-macro-dependencies.md)), and the last remaining crate (`nix`, for POSIX file locking and raw-mode termios) was replaced by ~180 lines of vendored, verified `unsafe extern "C"` bindings confined to `src/sys/` — the crate's sole `unsafe` carve-out, everywhere else is `#![deny(unsafe_code)]` ([ADR-0031](.openspec/adr/0031-vendor-nix-subset.md)).

This is a machine-checked claim, not a prose one: [`sqlite-rs.cdx.json`](sqlite-rs.cdx.json) is a [CycloneDX](https://cyclonedx.org/) SBOM generated from `Cargo.lock` (`make sbom`), and it has zero components. Build-time code execution is a real attack surface independent of what ships, though, so [`sqlite-rs-dev.cdx.json`](sqlite-rs-dev.cdx.json) (`make sbom-dev`) covers the full `Cargo.lock` closure — every test/build/bench-only crate, `scope`-tagged `optional` — for exactly that visibility; `make deny`/`make audit`/`cargo vet` already gate that same closure in CI.

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

**Version 0.18.5** — see [CHANGELOG.md](CHANGELOG.md). One minor version per completed plan phase.

| Phase | Version | Status |
|-------|---------|--------|
| V1 — Read core | 0.1.0–0.4.0 | ✅ Complete |
| V2 — Single-table queries | 0.5.0–0.8.0 | ✅ Complete |
| V3 — CRUD | 0.9.0–0.12.0 | ✅ Complete |
| V4 — JOINs & aggregates | 0.13.0 | ✅ Complete |
| V5 — Transactions | 0.14.0–0.15.0 | ✅ Complete |
| V6 — WAL & CTEs | 0.16.0–0.17.0 | ✅ Complete |
| V7 — Polish & compatibility | 0.18.x | ✅ Complete |

### Performance

5 of 7 benchmark queries beat or match the sqlite3 oracle (v3.53.4):

| Query | Ratio | Status |
|-------|------:|--------|
| point_lookup | 0.10× | 10× faster than C |
| filter_scan | 0.73× | beats oracle |
| full_scan | 0.83× | beats oracle |
| order_by_limit | 0.98× | parity |
| join | 1.86× | within 2× |
| group_by_agg | 5.1× | within 5× |
| correlated_subquery | 1.91× | within 2× |

See [docs/performance.md](docs/src/performance.md) for the full progression.

## Getting Started

### Build

```bash
cargo build --release
```

### Run

```bash
# Query a database
target/release/sqlite-rs query mydb.db "SELECT * FROM users"

# Interactive REPL
target/release/sqlite-rs repl mydb.db

# Dump schema and data
target/release/sqlite-rs dump mydb.db
```

### Documentation

```bash
# Build the docs site
make docs

# Serve locally
make docs-serve

# Generate rustdoc
cargo doc --open
```

### Testing

```bash
make test          # Unit + integration tests
make test-corpus   # Oracle parity tests (requires pinned sqlite3)
make lint          # Clippy + fmt
make bench         # Criterion benchmarks (requires pinned sqlite3)
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for build/test instructions, code style, and the PR process. Security issues should go through [SECURITY.md](SECURITY.md) rather than a public issue.
