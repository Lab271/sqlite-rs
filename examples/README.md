# Examples

Runnable samples showing how to use `sqlite-rs` as a library. This crate
exposes its parser/codegen/VM pipeline directly rather than an
ergonomic `Connection`/`prepare`/`bind` wrapper, so each example wires
those pieces together the same way the `sqlite-rs` CLI binary
(`src/bin/sqlite-rs/`) does.

Run any example with `cargo run --example <name>`.

- **`read_database.rs`** — opens an existing database file, lists its
  tables, and iterates every row of one table.
- **`query.rs`** — compiles a `SELECT` once and runs it with different
  bound `?1` parameter values.
- **`crud.rs`** — a full create/insert/update/delete cycle wrapped in an
  explicit `BEGIN`/`COMMIT` transaction.
- **`wal_mode.rs`** — switches a database to WAL journal mode, writes
  and reads through it, then checkpoints the WAL back into the main
  file. True multi-process concurrent readers/writer is out of scope
  for a single-binary example — see
  `tests/corpus/wal_concurrent_interop_test.rs` and
  `tests/corpus/wal_write_interop_test.rs` for that.

## Fixtures

`fixtures/sample.db` and `fixtures/empty.db` are small SQLite files
checked into the repo and built with the real `sqlite3` CLI. This
crate has no API to create a brand-new database file from nothing —
only to open an already-valid one — so `crud.rs` and `wal_mode.rs`
copy `empty.db` to a scratch path before writing to it.
