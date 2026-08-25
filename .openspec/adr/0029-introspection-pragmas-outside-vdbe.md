# 0029: Read-only introspection pragmas live outside the VDBE, as CLI-layer synthetic result sets

Date: 2026-08-25

## Context

#489 asked for 9 read-only, result-set-producing `PRAGMA`s
(`table_info`, `table_list`, `index_list`, `index_info`,
`database_list`, `schema_version`, `user_version`, `page_size`,
`page_count`). The only existing `PRAGMA` (`journal_mode`, #388) is a
side-effecting *write* pragma: parsed by the real grammar into a
`Pragma` AST node, compiled by `src/codegen/pragma.rs` into a
`SetJournalMode` VDBE opcode, and executed against an open
write-transaction `Pager`. That path has no concept of returning rows
at all — extending it to also produce result sets would mean teaching
the VDBE a new instruction family (or a virtual-table-like
pseudo-cursor) purely to re-surface data (`sqlite_master` rows, the
database header) the CLI layer already has fully decoded in memory
before any bytecode would run.

## Decision

The 9 pragmas are recognized by a small hand-rolled parser
(`src/bin/sqlite-rs/pragma_query.rs`, `parse_pragma_query` /
`execute_pragma_query`) that lives entirely at the CLI layer, alongside
`query.rs`/`repl.rs` — never touching `src/parser/grammar.rs`'s
`parse_pragma_stmt`, `src/parser/ast.rs`'s `Pragma` struct,
`src/codegen/pragma.rs`, or `src/vdbe/pragma.rs`. Each pragma's rows
are built directly from `schema::{TableSchema, ViewSchema}` and
`header::DatabaseHeader` — the same shape `EXPLAIN QUERY PLAN`'s
`SelectOutcome::Eqp` already established (#243): a synthetic in-memory
result set, printed the same pipe-delimited way, with no bytecode
compiled or executed. `query.rs` and `repl.rs` both check
`parse_pragma_query` first; anything it doesn't recognize (chiefly
`journal_mode`) falls through unchanged to the existing write-pragma
path.

## Alternatives rejected

- **A general virtual-table framework**, where `sqlite_master`,
  `pragma_table_info`, etc. are queryable as ordinary tables through
  the normal `FROM`/codegen/VDBE pipeline. This is the architecturally
  "proper" long-term shape (it's what SQLite itself does internally),
  but it's a large, speculative abstraction to introduce for 9
  fixed-shape, always-synthetic result sets with no `WHERE`/`JOIN`
  composability requirement in the ticket. Deferred until a real need
  for querying pragma output as a table (e.g. `SELECT * FROM
  pragma_table_info('t') WHERE pk > 0`) actually arrives.
- **Extending the `journal_mode` write-pragma path** to also produce
  result sets. Rejected because that path's entire shape (grammar ->
  AST -> codegen -> VDBE opcode -> `Pager` write transaction) assumes a
  side effect and an open write transaction; bending it to also return
  rows would complicate the one existing pragma to accommodate nine
  unrelated read-only ones.

## Consequences

- Adding a 10th read-only introspection pragma is cheap: one more
  `PragmaQuery` variant, one more match arm, no grammar/AST/codegen/VDBE
  changes.
- A pragma that needs real query composability (filtering, joining
  against it) is out of scope for this shape and would need the
  virtual-table alternative revisited — if that need arrives, this ADR
  should be superseded, not edited.
- `schema::column_defs`/`column_type` (`src/schema/ddl_reader.rs`)
  became `pub` (were `pub(crate)`) so the CLI-layer pragma module could
  reuse the existing column-definition splitting instead of
  re-deriving it — a small widening of `schema`'s public surface, not
  a new parsing implementation.
