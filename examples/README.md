# Examples

Runnable samples showing how to use `sqlite-rs` as a library.

Use **`sqlite_rs::api`** — `Connection`, `Statement`, `Rows`,
`Transaction`, `Value`. That is the supported surface (spec 013); the
parser/codegen/VM modules this crate also exports are the engine, and
carry no stability promise. See the API stability policy at the top of
`CHANGELOG.md`.

Run any example with `cargo run --example <name>`.

- **`read_database.rs`** — opens an existing file read-only, lists its
  tables, iterates every row of one, and shows a write being refused.
- **`query.rs`** — prepares a `SELECT` once and runs it with different
  bound `?1` values, then streams a multi-row result.
- **`crud.rs`** — a full create/insert/update/delete cycle, an explicit
  transaction, `last_insert_rowid`, the rows-affected count, and a
  rollback-on-drop.
- **`wal_mode.rs`** — switches a database to WAL journal mode, writes
  and reads through it, then checkpoints the WAL back into the main
  file. The one example still written against the engine rather than
  `api`: explicit checkpointing is a `pager` operation the facade does
  not expose (`Connection::pragma` can set `journal_mode`, but not
  trigger a checkpoint). True multi-process concurrent readers/writer is out of scope
  for a single-binary example — see
  `tests/corpus/wal_concurrent_interop_test.rs` and
  `tests/corpus/wal_write_interop_test.rs` for that.

## Fixtures

`fixtures/sample.db` and `fixtures/empty.db` are small SQLite files
checked into the repo and built with the real `sqlite3` CLI.
`read_database.rs` reads `sample.db`.

`empty.db` used to be load-bearing: this crate had no way to create a
database from nothing, so an example that wanted to write had to copy it
first. `Connection::open` now creates a valid empty database when the
path has no file, so `crud.rs` needs no fixture at all — and that the
result is a real SQLite database is checked against the pinned `sqlite3`
in `tests/corpus/bootstrap_oracle_test.rs`. `wal_mode.rs` still copies
`empty.db`, being engine-level.
