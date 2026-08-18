# SQLite Landscape: Rust Rewrites

**Updated:** 2026-08-18

## The Two Rust SQLite Rewrites

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

## Links

- Turso: https://turso.tech/
- Turso Database repo: https://github.com/tursodatabase/turso
- libSQL: https://github.com/tursodatabase/libsql
- sqlite-rs: https://github.com/iheitlager/sqlite-rs

## Related

- [cve-assessment.md](cve-assessment.md) — why memory safety matters (the CVE evidence)
- [README.md](../README.md) — sqlite-rs positioning and goals
