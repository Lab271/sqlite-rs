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

| # | Spec | Focus | Status |
|---|------|-------|--------|
| [001](specs/001-architecture/spec.md) | Architecture | System breakdown, layers, modules, line estimates | Draft |
| [002](specs/002-parser/spec.md) | Parser | SQL grammar, Lemon-equivalent, tokenizer | Draft |

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
