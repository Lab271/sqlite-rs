# Tiering

`sqlite-rs` is built as four tiers. Each tier depends only on the tiers
below it, and the tier-model contract tests in `tests/tiers/tier{0..3}.rs`
(spec `001-architecture`) claim completeness for each one.

| Tier | Layer | Responsibility | Key types |
|------|-------|----------------|-----------|
| 0 | Storage | File I/O, pages, B-tree, WAL | `Pager`, `BTree`, `Page` |
| 1 | Schema | DDL, catalog, type system | `Schema`, `TableDef`, `IndexDef` |
| 2 | Query | Parse, plan, optimize | `Parser`, `Planner`, `Plan` |
| 3 | Execution | VDBE, CLI, API | `Vm`, `Connection`, `Statement` |

`tier0.rs` is the never-droppable gate: it must never regress or gain an
`#[ignore]`. Tiers 1–3 gain active contracts as phases land — see the
per-value-block epics for which phase flips which stub.
