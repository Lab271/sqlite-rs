# OpenSpec — sqlite-rs

Specifications, architectural decisions, and design documents for sqlite-rs — a pure Rust implementation of SQLite.

## Goal

A SQLite-compatible database engine written entirely in Rust, providing:

- **Drop-in compatibility** with SQLite file format and SQL dialect
- **Memory safety** guarantees from Rust's ownership model
- **Thread safety** without data races
- **No C dependencies** — pure Rust, auditable, embeddable

## Architecture Philosophy

sqlite-rs follows SQLite's layered architecture. Each layer has one job and presents a well-defined abstraction to the layer above:

```
┌─────────────────────────────────────────┐
│           SQL Interface                 │  ← Public API
├─────────────────────────────────────────┤
│    Tokenizer  →  Parser  →  Analyzer    │  ← Frontend
├─────────────────────────────────────────┤
│         Code Generator (Planner)        │  ← Query planning
├─────────────────────────────────────────┤
│      Virtual Machine (VDBE)             │  ← Bytecode execution
├─────────────────────────────────────────┤
│            B-Tree                       │  ← Logical storage
├─────────────────────────────────────────┤
│            Pager                        │  ← Page cache + journaling
├─────────────────────────────────────────┤
│          WAL / Journal                  │  ← Durability
├─────────────────────────────────────────┤
│         OS Interface (VFS)              │  ← Platform abstraction
└─────────────────────────────────────────┘
```

## Specs

| # | Spec | Focus | Tier | Status |
|---|------|-------|------|--------|
| [001](specs/001-architecture/spec.md) | Architecture | System breakdown, tier model, layers, Tier 0 read-completeness | All | Draft (planned) |
| [002](specs/002-parser/spec.md) | Parser | SQL grammar, Lemon-equivalent, tokenizer, minimal DDL reader boundary | 1+ | Draft (planned) |
| [003](specs/003-file-format/spec.md) | File Format | Header, varints, serial types, encodings, read-only VFS | 0 | Draft — active (#9, #11) |
| [004](specs/004-corpus/spec.md) | Corpus & Oracle | Pinned oracle, fixture families, diff harness | 0 | Draft — active (#10) |
| [005](specs/005-assurance/spec.md) | Assurance | Four-pillar assurance stack, harness taxonomy, no-panic totality claim | All | Draft — active (#25, #26) |

## Grammar

[`grammar/sqlite.ebnf`](grammar/sqlite.ebnf) — the SQL grammar source of truth: EBNF re-derivation of SQLite's `parse.y` (pinned 3.53.4), every rule annotated with its V-block and parse.y origin. `make grammar-drift` validates all annotations against the pinned parse.y (downloads/caches it in `target/`). Grew out of spike 001's subset grammar (#63).

## Progress & Coverage Tracking

Follows the mvl convention: every requirement carries `**Implementation:**` and `**Tests:**` links (plus `**Corpus:**` where fixtures back it), and every requirement has `#### Scenario:` blocks in Given-When-Then form. `tools/assurance.py` parses the specs and assembles the case from three levels:

- **Traceability** — *Completeness (S→P):* does each requirement's implementation file exist? *Coverage (E→P):* scenario-weighted — a requirement with 5 scenarios and 1 test link scores 1/5, not 100%.
- **Evidence** — corpus fixtures present; cached line coverage (`make coverage` via cargo-llvm-cov). CI enforces a 75% line-coverage gate on every push/PR (`make coverage-gate`) and posts the per-file report as a sticky PR comment, so this evidence is refreshed and visible on every PR rather than only available locally (#20).
- **Verification** — `cargo test` / the oracle harness (not measured by the dashboard).

Requirements marked `(planned)` after the Implementation link describe future tiers and are excluded from scoring; specs on the current epic's critical path are active. As V-blocks progress, planned requirements flip to active and the dashboard tracks completion.

```bash
make assurance              # dashboard
make assurance VERBOSE=true # per-requirement detail
make assurance-gate         # CI gate at 75%
make coverage               # line coverage report (cargo-llvm-cov)
make coverage-gate          # CI gate: fail if line coverage < 75%
make traceability           # fast path, no I/O
```

## ADRs

| # | ADR | Status |
|---|-----|--------|
| — | — | — |

## Compatibility Target

- **SQLite version:** 3.45+ file format
- **SQL dialect:** SQLite SQL (not ANSI SQL)
- **File format:** Byte-compatible with `.sqlite` / `.db` files
- **API:** Rust-native, inspired by rusqlite but not a wrapper

## Development Strategy

1. **Parser first** — tokenizer + grammar using lemon-rs or lalrpop
2. **VDBE second** — bytecode VM with SQLite-compatible opcodes
3. **B-Tree third** — storage engine with page-level compatibility
4. **Pager/WAL last** — durability layer, crash recovery

Use SQLite's test suite (700:1 test-to-code ratio) as the oracle.

## Related Projects

| Project | Relationship |
|---------|--------------|
| [rusqlite](https://github.com/rusqlite/rusqlite) | Rust bindings to C SQLite (not a rewrite) |
| [lemon-rs](https://github.com/gwenn/lemon-rs) | Lemon parser generator for Rust + SQLite grammar |
| [limbo](https://github.com/tursodatabase/limbo) | Turso's SQLite-compatible Rust DB (similar goal) |
| [gluesql](https://github.com/gluesql/gluesql) | Pure Rust SQL, not SQLite-compatible |
