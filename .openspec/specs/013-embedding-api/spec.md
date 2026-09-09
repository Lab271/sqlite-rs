---
domain: embedding-api
version: 0.2.0
status: draft
date: 2026-08-28
---

# 013 — Embedding API

The public surface an application links against. Everything below is additive:
a rows-affected count, a connection and statement facade, a `Send + Sync`
handle, a durability contract, and a stability policy. No storage behavior, no
SQL surface, no opcode.

The engine primitives are already public and nearly sufficient --
`examples/query.rs` opens a database, compiles once and binds `?1` per
execution; `examples/crud.rs` writes inside a transaction -- so a consumer
willing to write glue can embed this crate today, and one does (SQE, an Iceberg
query engine that stores catalog pointers in SQLite; cited as *SQE* where its
measured need pins a decision). Requirement 1 is the one item on this list a
consumer cannot work around.

Decisions and rejected alternatives: ADR-0041 (where the API lives, and why
the `sqlx` driver stays out of tree), ADR-0043 (the failure surface: the error
type, the result codes, busy retry and schema refresh).

## Scope and inheritance

This spec defines only the surface. Every concern below is already specified
elsewhere and is not restated here.

| Concern | Defined in |
|---------|-----------|
| File locking, WAL reader marks, hot-journal handling, `VfsError::Locked` | spec 007 |
| Bound parameters, the `Variable` opcode, register allocation | spec 009, ADR-0015 |
| Write opcodes and their semantics | spec 010 |
| Value affinity, comparison, storage classes | spec 008 |
| Oracle diff harness, fixture families, corpus layout | spec 004, spec 005 |
| PRAGMA catalogue and priority tiers | plan.md, V7 |
| Recording a deliberate divergence from stock SQLite | ADR-0004 |
| `Rc`/`RefCell` page-source ownership, why `Vm` is not generic | ADR-0013, ADR-0017 |

Nothing here is in the tier model: the tiers rank SQL capability, this spec
ranks consumability. It sits outside the V1--V12 ladder because every block in
plan.md delivers SQL surface and this one delivers an API, and it is a
prerequisite for V7's stated demo ("point an existing tool ... at sqlite-rs and
have it work"), so it belongs as its own block before V8.

## The consumer this is drawn from

SQE uses this crate as a pointer store, not a query engine. Two tables written
only by its Iceberg catalog layer: `iceberg_tables` maps `(catalog_name,
table_namespace, table_name)` to a `metadata_location` string,
`iceberg_namespace_properties` maps a namespace property key to a value. One
row per Iceberg table, written at commit frequency, read by primary key. User
SQL never reaches it.

Two consequences that shape the requirements. Throughput is irrelevant, so
serialized access is acceptable and Requirement 4 is about reachability rather
than parallelism. And correctness is absolute, because each row points at a
table that may hold terabytes: an unenforced uniqueness constraint, a lost
compare-and-swap or a non-durable commit makes a table unreachable rather than
slow. Requirements 1 and 5, plus the composite-key prerequisite, are the
correctness core; the rest is safety and ergonomics.

That consumer is now adding a second, different use of the same crate: attaching
an arbitrary SQLite database and exposing its tables as queryable relations, so
a user can join one against an Iceberg table. Arbitrary schemas, arbitrary
affinities, arbitrary row counts. Requirement 7 exists because of it, and it is
the only requirement here driven by a read path rather than a pointer store.

## What is missing today

Each line is checkable against the tree at 0.18.10.

1. **A rows-affected count.** Nothing in `src/vdbe/` reports how many rows an
   `INSERT`/`UPDATE`/`DELETE` changed. The only capability gap here.
2. **A facade.** No `Connection`, `Statement` or `Transaction`; the caller
   assembles pager, header, `Program` and a positional `Vec<Value>` by hand.
3. **A `Send + Sync` handle.** `Rc<dyn PageSource>` and `Rc<RefCell<Pager>>`
   are `!Send`, and ownership does not change that, so an async trait cannot
   hold the engine at all. A second `!Send` was missed on first writing and is
   now closed: `Value::Text`/`Blob` held `Rc` payloads, so a result row --
   the one thing that has to *leave* a worker thread -- could not cross a
   thread boundary either. Both this spec and ADR-0041 originally attributed
   the problem to the pager alone. `Value` is `Arc`-backed and `Send + Sync`
   as of ADR-0039, which leaves only the pager half, and the pager half is
   what Requirement 4's worker thread is for.
4. **A creation API.** `DatabaseHeader::new_empty_page1` is public
   (`src/header.rs:295`) but no API offers it, so `examples/README.md` records
   that the examples copy `fixtures/empty.db` instead.
5. **Named parameters.** `:name`, `@name`, `$name` reach the always-NULL stub
   ADR-0015 left in place, which a public facade would make reachable.
6. **A durability contract in writing** -- not the mechanism, which
   exists. `Pager` syncs (`src/pager.rs:589,597,697,782`) and
   `PRAGMA synchronous` is fully implemented: the query form and all three
   levels, with a decided per-level fsync-skip policy (#645, ADR-0036,
   `src/vdbe/pragma.rs:79`). This spec originally said it "has no handler",
   which was already false when written. What is genuinely absent is a
   *stated* guarantee -- what a consumer is promised at commit, per level --
   so a reader has to derive it from `Pager`'s source.
7. **Incremental row access, at the facade.** `execute_with_db` and
   `execute_with_db_and_params` return `Vec<Vec<Value>>`
   (`src/vdbe/exec.rs:1073, 1093`), so those entry points still materialize a
   result set before the caller sees a row. The engine half is no longer
   missing: `vdbe::Execution` (ADR-0040) is a public streaming primitive, and
   `run()` is now a wrapper that collects it, so batch and streaming are the
   same loop. What is still absent is a consumer-facing step API -- which is
   Requirement 7, restated below against that primitive rather than against
   `execute_with_db`.

## Prerequisites owned elsewhere

Two SQL-semantics gaps block a consumer and belong to V3/V7, not to this spec.
They are listed so nobody plans around the wrong gap.

- **`CREATE TABLE IF NOT EXISTS` is ignored after parsing.** `if_not_exists`
  appears only in `src/parser/grammar.rs` and `src/parser/printer.rs`.
- **A composite `PRIMARY KEY`/`UNIQUE` table constraint: the maintenance half
  is fixed, the creation half is not.** As of #685 an existing
  `sqlite_autoindex_*` is recovered from the owning table's DDL and maintained,
  uniqueness is enforced against it, and an autoindex this reader cannot
  interpret makes the table read-only rather than writable-and-corrupting
  (spec 010 Requirement 8). What remains is emitting one on `CREATE TABLE`,
  tracked as #687, so a table created *here* with a declared composite key
  still lacks its index and the oracle still calls the file "malformed (11)" on
  any write to it. The description below is the original finding, kept because
  it is what the requirement was written against. Now
  measured rather than inferred (0.18.5 against the pinned `sqlite3` 3.53.4
  oracle), and it
  is worse than a missing feature: writing into a stock-created table with a
  declared composite PK leaves rows out of the autoindex, after which the oracle
  undercounts and `integrity_check` reports rows missing, while the write
  returns success. Creating the same DDL here yields a file the oracle calls
  "malformed (11)" on any write. A table with no declared PK and a named
  `CREATE UNIQUE INDEX` round-trips cleanly in both directions with uniqueness
  enforced by both. The mechanism is `src/schema/ddl_reader.rs`'s deliberate
  skip of an index whose `sqlite_master.sql` is NULL, which is right for a read
  and data loss for a write. Spec 010 Requirement 8 states the write-side rule;
  creating the autoindex stays V3/V7's. **This is the highest-priority item in
  or around this spec**, because it is a silent-corruption bug against a valid
  SQLite file rather than an ergonomic gap, and because Requirement 6's
  byte-identity scenario cannot pass while it stands.

  *SQE*'s response, for reference: its catalog schema drops the declared
  composite primary key in favour of a named unique index, and its adapter
  refuses writes to any catalog carrying an `sqlite_autoindex_*`. The second
  workaround comes out now -- #685 makes writing to such a catalog safe, which
  is the adoption direction SQE actually needs. The first waits on #687,
  because a catalog this crate creates still needs the named index.

## Requirements

### Requirement 1: A Rows-Affected Count [MUST]

The API MUST report how many rows the last `INSERT`, `UPDATE` or `DELETE`
changed, as `sqlite3_changes()` does, following SQLite's rules: a statement
returning no rows does not reset it, and it counts rows changed rather than
examined.

Every optimistic-concurrency scheme is built on the distinction between an
`UPDATE` that matched and one that did not. *SQE* swaps a table's metadata
pointer with a conditional `UPDATE` and treats zero rows affected as a lost
race; without the count that becomes SELECT-then-UPDATE in a transaction, sound
only while the consumer guarantees a single writer, and every consumer
reinvents it.

The rule with the surprising semantics is the *retention* one, and it belongs
to the connection rather than the engine: `sqlite3_changes()` reports what the
last *counting* statement did, so a `SELECT` or a DDL statement in between must
leave it standing. The engine half is `StepOutcome::changes` (#692), an
`Option<u64>` whose `None` means "not a counting statement, leave the stored
value alone" — `Some(0)` and `None` are different answers and the difference is
the whole point.

`Connection::last_insert_rowid` follows the same retention rule and is
specified here with it: a row inserted into a table with a surrogate key is
unaddressable until the caller learns its rowid. It is driven by
`OPFLAG_LASTROWID` on `P5` rather than by the `Insert` opcode, because an
`UPDATE` also emits an `Insert` and must not move the value.

**Implementation:** `src/api.rs::Connection::changes`, plus
`::Connection::last_insert_rowid`

**Tests:** `tests/unit/api_changes_test.rs`,
`tests/unit/vdbe_last_insert_rowid_test.rs`,
`tests/corpus/last_insert_rowid_oracle_test.rs`

#### Scenario: A conditional update reports whether it matched

- GIVEN a row with `metadata_location = 'a'`
- WHEN `UPDATE t SET metadata_location = 'b' WHERE metadata_location = 'a'`
  runs, then the identical statement runs again
- THEN the first reports one row changed, the second reports zero, and the
  pinned oracle agrees with both

**Tests:** `tests/unit/api_changes_test.rs::conditional_update_reports_match`, `tests/corpus/api_oracle_test.rs::api_rows_affected_and_resulting_file_match_the_oracle`

#### Scenario: A SELECT does not clobber the count

- GIVEN a `DELETE` that removed two rows
- WHEN a `SELECT` returning no rows runs next
- THEN the count still reports two

**Tests:** `tests/unit/api_changes_test.rs::select_does_not_clobber_count`

#### Scenario: An insert's rowid is retained until the next insert

- GIVEN an `INSERT` into a table with an `INTEGER PRIMARY KEY` that supplied
  its own key
- WHEN an `UPDATE`, a `DELETE` and a `SELECT` run next
- THEN the last-insert rowid still reports the supplied key, and the pinned
  oracle agrees at every step

**Tests:** `tests/unit/api_changes_test.rs::last_insert_rowid_is_retained_across_statements`, `tests/corpus/last_insert_rowid_oracle_test.rs::last_insert_rowid_matches_the_oracle`

### Requirement 2: Connection, Open or Create [MUST]

The API MUST open an existing database and, when asked, create a valid empty
one. Modes MUST distinguish read-only, read-write and read-write-create, and
MUST NOT create a file when create was not requested. Locks are spec 007's; the
handle MUST release them on drop, which `Pager` already does.

*SQE* opens its catalog as `sqlite://<path>?mode=rwc` and expects the file to
appear on first use; a first-run laptop has no `empty.db` to copy.

Creation writes `DatabaseHeader::new_empty_page1` (`src/header.rs:319`), the
same bootstrap the CLI's `exec` uses. That this produces a database stock
`sqlite3` accepts was, until this spec, **never verified**: both
`tests/tiers/tier2.rs::seed_db` and `tests/corpus/cli_write_test.rs::seed_db`
prefer the oracle to build their fixture and fall back to our own path only when
no oracle is installed, so the two never ran together.

**Read-only is enforced per statement, not by the pager**, and that is a
deliberate divergence recorded under ADR-0004. `Pager::open` calls
`Vfs::open_write` unconditionally (`src/pager.rs:419`) and there is no
read-only pager; the read-only page source that does exist (`VfsPageSource`)
bypasses `Pager` entirely and so merges no WAL frames, which would silently
serve stale data for a WAL database. Refusing writes above the pager is the
honest option until spec 007 grows a read-only pager. The guard keys on
`OpenWrite` and the DDL opcodes rather than on `Insert`/`Delete`, because those
two also target ephemeral cursors: a materialized FROM-subquery emits an
`Insert` in a plain `SELECT`.

An in-memory database (`open_in_memory`) is also provided, backed by
`MemoryVfs`, so it exercises the same pager, journal and b-tree code a file
does rather than a separate path.

**Implementation:** `src/api.rs::Connection::open`, plus `::open_with`,
`::open_in_memory` and `::OpenMode`

**Tests:** `tests/unit/api_connection_test.rs`,
`tests/corpus/api_oracle_test.rs`, `tests/corpus/bootstrap_oracle_test.rs`

#### Scenario: Create produces a database the oracle reads

- GIVEN a path with no file at it
- WHEN opened `ReadWriteCreate`
- THEN a valid database exists and the pinned oracle reports an empty schema

**Tests:** `tests/corpus/api_oracle_test.rs::create_then_oracle_reads_empty_schema`, `tests/unit/api_connection_test.rs::open_creates_a_database_that_reopens_and_reads_back`

#### Scenario: Without create, nothing is written

- GIVEN a path with no file at it
- WHEN opened `ReadWrite`
- THEN it fails and no file exists afterwards

**Tests:** `tests/unit/api_connection_test.rs::readwrite_does_not_create`, `tests/corpus/api_oracle_test.rs::readwrite_creates_nothing_the_oracle_can_find`

#### Scenario: A created database is valid at every page size

- GIVEN a page 1 built by `new_empty_page1` for each supported page size,
  including the 65536 case that cannot be stored literally in the 16-bit field
- WHEN the pinned oracle opens it
- THEN `integrity_check` is `ok`, the page size round-trips, the schema is
  empty, and the oracle can then grow the file

**Tests:** `tests/corpus/bootstrap_oracle_test.rs::an_empty_database_we_build_is_valid_at_every_page_size`

#### Scenario: Read-only refuses every write and permits every read

- GIVEN a connection opened `ReadOnly`
- WHEN a `SELECT` whose plan materializes a FROM-subquery runs, and then an
  `INSERT`, `UPDATE`, `DELETE`, `CREATE` and `DROP` are each attempted
- THEN the read succeeds, every write is refused with `SQLITE_READONLY`, and
  the file is unchanged

**Tests:** `tests/unit/api_connection_test.rs::readonly_reads_but_refuses_every_write`, `tests/unit/api_connection_test.rs::readonly_refusal_reports_sqlite_readonly`

### Requirement 3: Statement Handle [MUST]

The API MUST expose a statement that owns its compiled `Program` and its
parameter slots: bind positional `?`/`?NNN` (spec 009's `Variable` opcode),
then read rows as typed values by index and by name over spec 008's storage
classes, without the caller naming `Program`, registers or cursors. Named
parameter forms MUST be rejected at prepare time rather than reaching execution
as ADR-0015's always-NULL stub.

The value is not speed. *SQE* issues about a dozen statements at commit
frequency, so compiling once saves nothing measurable; a handle owning its slots
is what stops a transposed argument list writing a valid row that points at the
wrong table.

Three things beyond the handle itself belong here, because they are what stop a
caller getting silently wrong answers rather than errors:

- **Arity is checked exactly**, on every execution, and a mismatch is
  `Error::ParamCount`. Stricter than stock SQLite, which leaves an unbound
  parameter NULL — recorded as an ADR-0004 divergence, because refusing what
  SQLite accepts is the safe direction and catching a transposed argument list
  is this requirement's stated value.
- **`execute_batch`** runs a multi-statement script, and `execute`/`prepare`
  refuse one with `Error::MultipleStatements`. A script is what schema setup
  is; silently running only its first statement is not an option.
- **By-name column access is scoped to single-table `SELECT`s.**
  `result_column_names` (`src/codegen/prepare.rs:181`) returns `column1`,
  `column2`, … for a join or a compound, so by-name access will not find a
  base-table name there. By-index access is unaffected. Stated rather than
  hidden; the fix belongs in the name resolver and is not this spec's.

**Implementation:** `src/api.rs::Statement`, plus `::Row`, `::FromValue` and
`::Connection::execute_batch`

**Tests:** `tests/unit/api_statement_test.rs`,
`tests/unit/param_binding_test.rs`, `tests/unit/api_streaming_test.rs`

#### Scenario: One compile, many bindings

- GIVEN a prepared `SELECT name FROM t WHERE id = ?1`
- WHEN executed with 1, 2 and 3 bound
- THEN each returns that row's name and compilation happened once

**Tests:** `tests/unit/api_statement_test.rs::compile_once_bind_many`

#### Scenario: A named parameter is refused, not silently NULL

- GIVEN `SELECT * FROM t WHERE id = :id`
- WHEN prepared
- THEN preparation fails naming the unsupported form

**Tests:** `tests/unit/api_statement_test.rs::named_param_is_refused_at_prepare`, `tests/unit/param_binding_test.rs::named_parameters_are_refused_rather_than_bound_to_null`

#### Scenario: A wrong argument count is refused, not padded with NULLs

- GIVEN a prepared statement with two placeholders
- WHEN one value, and then three, are bound
- THEN each is refused with `SQLITE_RANGE`, nothing is written, and the handle
  still works afterwards

**Tests:** `tests/unit/api_statement_test.rs::a_wrong_argument_count_is_refused_every_time`, `tests/unit/api_changes_test.rs::the_wrong_number_of_parameters_is_refused`

#### Scenario: Bound values of every storage class round-trip

- GIVEN an `INSERT` binding an integer, a real, text, a blob and NULL
- WHEN the file is read back through the pinned oracle
- THEN the values and their `typeof()` match the oracle running the same
  statements with the values inlined

**Tests:** `tests/corpus/api_oracle_test.rs::parameterised_writes_match_the_oracle`, `tests/unit/api_streaming_test.rs::every_storage_class_reads_back_as_its_rust_type`

### Requirement 4: A `Send + Sync` Handle Over an Owned Worker Thread [MUST]

The handle MUST be `Send + Sync` so a pool, an async task or a trait demanding
those bounds can hold it, while the engine state stays `Rc`/`RefCell` per
ADR-0017.

Both cannot hold by making the connection type `Send`: `Rc` is not `Send` and
ownership does not change that, so the compiler rejects the shape. Of the two
achievable designs -- an `Arc`/lock refactor of `Pager` and `PageSource`, which
ADR-0013 and ADR-0017 rejected on read-path cost, or a worker thread owned by
the connection -- this requirement specifies the second.

ADR-0039 narrows the scope of that choice without reopening it. `Value`'s
payloads are now `Arc`, measured at no read-path cost, so rows themselves are
`Send` and need no copy at the boundary; ADR-0013 and ADR-0017 were only ever
about the pager, and their subject matter is untouched. The worker thread is
still required, and still for exactly the reason above -- but it now hands
rows across rather than serializing them. `sqlx`'s own SQLite
driver does the same for a C `sqlite3*`
(`sqlx-sqlite-0.9.0/src/connection/worker.rs`: a spawned thread behind a `flume`
channel, one per connection), so implementing it here gives every consumer once
what each would otherwise write.

One further constraint makes the worker thread the only *expressible* design
rather than merely the preferred one, and it was not known when ADR-0041 was
written: `make check-mvl-limit` forbids named lifetime parameters in `src/`, so
no type in `src/api.rs` may hold an `Execution` — it borrows its `Program`. The
execution has to live inside a single worker stack frame, which is exactly what
this design gives it. (`src/codegen/stmt/insert.rs::ColumnSource` already
carries a comment making the same trade for the same reason.)

The thread MUST terminate on drop, and a request after it dies MUST error rather
than block.

**Coordination between connections in one process is currently broken, and
that is a finding rather than a design choice.** This requirement previously
asserted it was "spec 007's file locks, as between processes". It is not: POSIX
`fcntl` locks are scoped to `(process, inode)`, which `src/vfs/lock.rs:96`
documents and `FileLockState::check_reserved_lock` states outright ("whether
some *other* process currently holds a write lock"). Two connections on one
file in one process therefore do not exclude each other. Measured: connection A
takes `BEGIN IMMEDIATE` and inserts a row; connection B's insert returns
`Ok(1)`; A commits; B's row is gone, and `integrity_check` reports `ok`. A
write that reported success is silently discarded.

Stock SQLite closes this with `unixInodeInfo` in `os_unix.c` — a process-global
registry keyed by `(device, inode)` with its own mutex and lock counts, so two
connections in one process serialize like two processes. There is no equivalent
here. It is a pre-existing engine gap, but this requirement makes it reachable:
the whole point of a `Send + Sync` handle is that a *pool* can hold it. Tracked
as a ratchet
(`tests/unit/api_durability_test.rs::in_process_connections_lock_against_each_other`,
`#[ignore]`d) and needs its own ticket. No shared cache, no other global state.

**Implementation:** `src/api.rs::Connection`

**Tests:** `tests/unit/api_threading_test.rs`

#### Scenario: The handle is shared across threads

- GIVEN a connection opened on thread A
- WHEN its handle is cloned into several threads that each run a query
- THEN every query succeeds, and a compile-time assertion proves the handle is
  `Send + Sync`

  The original third clause was "and all engine access happened on the
  connection's thread", which is not falsifiable from outside — there is no
  observable that distinguishes it. The testable statement of the same intent
  is that **no engine type appears in the handle's public signature**, which
  Requirement 6's surface test already asserts, plus the compile-time bound
  above.

**Tests:** `tests/unit/api_threading_test.rs::handle_is_send_sync`, `tests/unit/api_threading_test.rs::a_shared_reference_works_across_threads`

#### Scenario: The thread is released, and a dead engine errors

- GIVEN a loop that opens and drops connections
- WHEN it finishes
- THEN every cycle's write committed and was visible to the next, which
  requires each worker to have terminated and released its file locks before
  the next connection opened

  "The thread count is unchanged" is asserted structurally instead of by
  counting threads, for which there is no portable API here: `Drop` closes the
  request channel and then *joins*, so a worker that failed to terminate would
  hang the drop rather than leak. The test completing is the assertion. A
  request on a dead worker returning `Error::ConnectionClosed` rather than
  blocking is a property of the channel — and note it is unreachable through
  the public API by construction, since the handle owns the sender that keeps
  the worker alive.

**Tests:** `tests/unit/api_threading_test.rs::worker_thread_joins_on_drop`, `tests/unit/api_threading_test.rs::dropping_one_clone_leaves_the_rest_working`

### Requirement 5: Transactions and a Stated Durability Contract [MUST]

The API MUST expose `BEGIN`/`COMMIT`/`ROLLBACK` (deferred, immediate,
exclusive), MUST thread the autocommit state `execute_transaction_step` already
returns so a multi-statement transaction is one unit, and MUST roll back a
transaction handle dropped without commit.

It MUST also state what is durable at commit and honor `PRAGMA synchronous` at
least to distinguish FULL from OFF. **The `synchronous` half of this is
already done** -- #645/ADR-0036 implement all three levels with a decided
fsync-skip policy per level, which is more than "at least FULL from OFF"
asks for. Sync points exist (`src/pager.rs:589,597,697,782,811,845`) and
there is now a way to trade them. What remains for this requirement is the
written guarantee, the transaction surface, and the retryable-error handling
below. A consumer storing pointers to data it cannot
otherwise find needs that in writing: *SQE*'s file holds the metadata pointer
for every table in a warehouse, so a commit returning before it is durable turns
a power failure into tables that exist on object storage and are unreachable.
Where a PRAGMA is accepted without being honored, record it as a divergence
under ADR-0004.

Retryable errors belong here too: spec 007's `VfsError::Locked` MUST surface as
a distinct, documented busy variant with `is_retryable()`, and a busy timeout
MUST be settable per connection. The error type MUST also expose SQLite's own
result codes — `sqlite_code()` for the primary and `extended_sqlite_code()` for
the extended, mirroring `sqlite3_errcode()`/`sqlite3_extended_errcode()` — so a
consumer can distinguish a UNIQUE violation (2067) from any other constraint
failure without matching on message text.

Two constraints on how far the timeout goes:

- **It retries only outside an explicit transaction.** In autocommit the
  statement *is* the transaction, so rolling the pager back and re-running it
  is a faithful retry. Inside one it is not: the statement's mutations share
  the pending set with every earlier statement's, so re-running one would
  double-apply it. A busy inside a transaction is the transaction's to retry,
  which is what stock SQLite does with `SQLITE_BUSY` at `COMMIT`.
- **The rollback before each retry is load-bearing.** `Pager::flush`
  (`src/pager.rs:524`) surfaces `VfsError::Locked` before any byte is
  journaled and leaves `dirty` intact, so without clearing it a retried
  `INSERT` would land once per attempt.

`Connection::pragma` covers what a pool sets and what durability requires. It
**cannot** serve the introspection pragmas (`table_info` and the other eight):
those live in `src/bin/sqlite-rs/pragma_query.rs` per ADR-0029, inside the
binary. `Connection::table_names` covers the one introspection need a consumer
actually has, reading the decoded catalog — `sqlite_master` is not queryable
through `SELECT` at all today (`resolve_from_table_schema` does not resolve
it). The PRAGMA catalogue is plan.md's V7.

**Implementation:** `src/api.rs::Transaction`, plus `::Connection::pragma`,
`::Connection::set_busy_timeout` and `::Error`

**Tests:** `tests/unit/api_transaction_test.rs`,
`tests/unit/api_durability_test.rs`,
`tests/corpus/api_durability_oracle_test.rs`

#### Scenario: Dropped transaction rolls back

- GIVEN an open transaction with one `INSERT` applied
- WHEN the handle drops without `commit()`
- THEN the row is absent and the pinned oracle agrees

**Tests:** `tests/unit/api_transaction_test.rs::drop_rolls_back`, `tests/unit/api_transaction_test.rs::an_early_return_rolls_back_and_leaves_the_connection_usable`

#### Scenario: A committed transaction survives a hard kill

- GIVEN a transaction committed under `synchronous = FULL`
- WHEN the process is killed without unwinding and the database reopened
- THEN the rows are present and `integrity_check` passes under the oracle

  What that does *not* establish, stated so the durability claim is not
  overread: SIGKILL leaves the kernel page cache intact, so it cannot
  distinguish `FULL` from `NORMAL` or `OFF`. It establishes that the commit was
  complete in the file rather than buffered in the process, and that an abrupt
  death leaves nothing malformed. Separating the `synchronous` levels needs a
  power cut or a crash-injecting VFS, which is
  `tests/corpus/crash_torture_test.rs`'s regime.

**Tests:** `tests/corpus/api_durability_oracle_test.rs::commit_survives_hard_kill`

#### Scenario: Busy is retryable and distinguishable

- GIVEN a second **process** holding the write lock
- WHEN a write is attempted
- THEN the error is the busy variant, `is_retryable()` is true, `sqlite_code()`
  is 5, and a retry after release succeeds

  A second *process*, not a second connection: two connections in one process
  do not exclude each other at all — see Requirement 4. Using the pinned
  `sqlite3` as the lock holder also makes the claim stronger, since the lock
  protocol is then honoured against stock SQLite rather than only against
  ourselves.

**Tests:** `tests/corpus/api_durability_oracle_test.rs::busy_is_retryable`, `tests/corpus/api_durability_oracle_test.rs::a_retried_statement_succeeds_exactly_once`

### Requirement 6: Published Surface, Stability Policy, and Acceptance [MUST]

The crate MUST state which modules are the supported surface and which are
implementation detail, and the facade MUST cover everything those internals
offer a consumer, Requirement 1 included, so nobody is forced back down a layer.
Today `src/lib.rs` exports the engine (`btree`, `codegen`, `dump`, `pager`,
`parser`, `planner`, `vdbe`, `vfs`, ...) while `CHANGELOG.md` says "Pre-1.0:
minor bumps may break the public API", so a consumer wiring `dump::open` to
`execute_transaction_step` builds on items carrying no promise and reasonably
hidden once `src/api.rs` exists. *SQE* pins an exact version and confines every
`sqlite_rs::` reference to one module for this reason; that is a workaround for
a missing policy, not a substitute.

Acceptance is spec 004's harness, not a new one: a consumer statement set
becomes a fixture family, diffed against pinned `sqlite3` 3.53.4. The first
family is *SQE*'s catalog, and the whole list is `CREATE TABLE IF NOT EXISTS`
with a three-column composite `PRIMARY KEY`, `INSERT` with four bound
parameters, `SELECT ... UNION` over two namespace sources, `LIMIT 1` existence
probes, a conditional `UPDATE`, and `DELETE`. Every statement in it lands in V2
through V4. The gap was never SQL coverage.

Acceptance lands in two parts, because one of them is blocked on bugs this
spec does not own:

- **Part A** — the API's own oracle-diff family: rows-affected counts, the
  resulting file read back through the pinned `sqlite3`, parameterised writes
  across every storage class, and streamed reads compared row by row. On the
  tree now.
- **Part B** — *SQE*'s literal statement list as a fixture family. The list
  above includes `CREATE TABLE IF NOT EXISTS` with a three-column composite
  `PRIMARY KEY`, and both halves of that are currently broken: #697 (the
  `IF NOT EXISTS` guard is ignored, duplicating the `sqlite_master` row and
  leaking a page) and #687 (no `sqlite_autoindex_*` is created for a declared
  composite PK, so the file is malformed to stock `sqlite3` before any write).
  A version using *SQE*'s own named-unique-index workaround passes today and
  ships separately; the literal-DDL version is a ratchet on those two tickets.

**Implementation:** `src/lib.rs` — module docs, plus `CHANGELOG.md`'s API
stability policy and `src/api.rs`'s re-export of `Value`

**Tests:** `tests/unit/api_surface_test.rs`, `tests/corpus/api_oracle_test.rs`

#### Scenario: The facade needs no escape hatch

- GIVEN a consumer using only the items this spec defines
- WHEN it creates a database, prepares and binds a statement, reads rows, runs a
  transaction and reads the rows-affected count
- THEN it compiles without naming `pager`, `vdbe`, `codegen`, `dump` or `btree`

  Compiling is the assertion, and on its own it would rot: a future capability
  gap "fixed" by importing an engine module would keep the test passing while
  this requirement became silently false. So a second test reads the test
  file's own source and fails if any of the fourteen engine modules is named
  in it.

**Tests:** `tests/unit/api_surface_test.rs::facade_is_sufficient_alone`, `tests/unit/api_surface_test.rs::this_file_names_no_engine_module`

#### Scenario: The consumer corpus matches the oracle

- GIVEN a consumer statement set run through this API and through the oracle
- THEN both produce identical rows in order, identical rows-affected counts, and
  identical files modulo documented header fields

  The documented header fields are exactly three, asserted exhaustively rather
  than waved at: the change counter (offset 24), version-valid-for (92) and the
  SQLite version number (96). All three are absent from `DatabaseHeader`, so
  they read as zero in a file we create. The change counter never advancing is
  a real interop limitation — another SQLite connection holding a cached image
  cannot learn our writes happened — but not a malformation, which is why
  `integrity_check` misses it and a byte-level assertion is needed.

  This is Part A. Part B, *SQE*'s literal DDL, is a ratchet on #687 and #697.

**Tests:** `tests/corpus/api_oracle_test.rs::api_rows_affected_and_resulting_file_match_the_oracle`, `tests/corpus/api_oracle_test.rs::queried_rows_match_the_oracle`, `tests/corpus/bootstrap_oracle_test.rs::our_fresh_header_differs_from_the_oracles_only_in_fields_we_do_not_model`

### Requirement 7: Incremental Row Access [MUST]

A statement MUST yield rows incrementally, as `sqlite3_step()` does. Today
`execute_with_db` and `execute_with_db_and_params` return `Vec<Vec<Value>>`, so
the engine allocates an entire result set before the caller sees the first row,
and there is no step or iterator API to fall back to.

For a pointer store that is invisible: a dozen rows, once per commit. For any
consumer reading a database as a data source it is the difference between a
usable API and an unusable one. *SQE* is adding exactly that use, attaching
arbitrary SQLite files so a user can query and join their tables; against a
million-row table the current shape materializes the whole table before the
first batch exists. Its interim answer is a configurable row ceiling with an
error beyond it, which is honest and narrow, and it comes off when this lands.

Memory MUST be bounded by the rows the caller has actually pulled, not by the
result set, and abandoning a partially-read statement MUST release its resources
and its cursors without waiting for the rest.

Not an `Iterator` impl: collapsing `Result<Option<Row>, Error>` into
`Option<Result<Row, Error>>` to fit the trait makes "the stream ended" and "the
stream failed" the same shape at the call site. ADR-0040 rejected an `Iterator`
on `Execution` for this reason and the reasoning carries.

**Holding an undrained `Rows` blocks the connection**, and that follows from
Requirement 4's serialized access rather than being incidental: the engine
stays inside one execution until the result is drained or the handle dropped,
so no other statement on that connection runs meanwhile. A result that fits in
one channel batch completes and frees the worker whether it is read or not — a
deliberate mitigation for the easy accident of preparing a query and forgetting
it — but a larger one parks the worker until the handle goes. Dropping it
releases immediately, so this is a wait rather than a deadlock.

**Implementation:** `src/api.rs::Rows::next_row`, built on
`src/vdbe/exec.rs::Execution::next_row` rather than on `execute_with_db` --
#682 found the ordering matters, because a facade retrofitted onto the
materializing entry point cannot be made incremental afterwards

**Tests:** `tests/unit/api_streaming_test.rs`,
`tests/corpus/api_oracle_test.rs`

#### Scenario: A large result is read without materializing it

- GIVEN a table with a row count well above any sensible buffer
- WHEN the first ten rows are read and the statement is dropped
- THEN peak allocation is **independent of the table's row count** -- flat as
  the table grows, rather than proportional to it -- and the ten values match
  the pinned oracle's first ten

  Stated that way deliberately. "Proportional to the ten rows" is not
  satisfiable by a correct implementation and was the original wording: peak
  heap for a streaming read is dominated by the page cache, not the rows
  pulled, so it is a floor rather than a slope. Measured on 1,000,000 rows
  (spike 014, #682): 137.7 MB materialized against 8.68 MB streamed, and the
  8.68 MB does not move with the result size. The whole floor is one constant,
  `DEFAULT_PAGE_CACHE_CAPACITY` (`src/pager.rs:63`) -- 2000 pages gives
  8.68 MB, 256 gives 1.10 MB, 64 gives 291 KB -- so a caller who needs the
  floor lower has a knob, at ~4.5% streaming throughput for the smallest.
  Independence from result size is the property a consumer actually needs, and
  unlike proportionality it is testable.

  There are **two** constants, not one. The page-cache floor above, and the
  channel buffer: peak is roughly four batches of `CHUNK_ROWS`
  (`src/api.rs`) — one being filled on the worker, two in the channel's
  `CHUNK_SLOTS`, one held by the caller. Both are independent of the result
  size, which is what makes the property hold.

  One caveat the test had to be corrected for: **the plan must be
  non-blocking.** An `ORDER BY` with no usable index is a blocking operator —
  the sorter consumes every row before emitting the first — so time-to-first-row
  there is genuinely linear and streaming cannot change it. Stock SQLite sorts
  the same way. A first draft of this test measured a sorted plan and failed at
  33x on a 50x larger table, correctly.

**Tests:** `tests/unit/api_streaming_test.rs::partial_read_is_bounded`, `tests/unit/api_streaming_test.rs::blocking_plans_are_linear_by_nature`

#### Scenario: Abandoning a statement releases it

- GIVEN a statement read halfway
- WHEN it is dropped
- THEN its cursors are released and a subsequent write on the same connection
  proceeds

**Tests:** `tests/unit/api_streaming_test.rs::abandoned_statement_releases_cursors`, `tests/unit/api_streaming_test.rs::dropping_a_large_unread_result_releases_the_connection`, `tests/unit/api_streaming_test.rs::an_unread_small_result_does_not_block_the_next_statement`

### Requirement 8: A Prepared Statement Must Not Run Against a Changed Catalog [MUST]

A statement compiled before a schema change MUST NOT execute against the old
plan: it MUST be recompiled, or the execution MUST fail. It MUST NOT read a
recycled root page.

This is not a caching concern. A compiled program addresses tables by root
page, and `DROP` returns that page to the freelist for a later `CREATE` to
reuse — so running a stale program can read a page that now belongs to a
different table, with no error anywhere. The same applies to indexes in the
other direction: a write compiled before `CREATE INDEX` maintains no index
entries, leaving rows in the table that the index does not have, which is
exactly the malformation #685 was about.

Recompiling is what `sqlite3_prepare_v2` does on `SQLITE_SCHEMA`, and is what
this specifies; failing would be safe too, but pushes a retry loop onto every
caller for something the connection can do itself. If recompilation fails
because the statement no longer compiles at all, the error MUST be reported and
MUST be repeatable rather than one-shot.

The count of automatic recompilations MUST be observable, mirroring SQLite's
`SQLITE_STMTSTATUS_REPREPARE` ("the number of times that the prepared statement
has been automatically regenerated due to schema changes", `sqlite3.h:9274` at
the pinned 3.53.4). That is also what makes Requirement 3's "compilation
happened once" a checkable claim rather than an assertion about internals.

**Implementation:** `src/api.rs::Statement::reprepare_count`, plus
`::Connection::prepare`

**Tests:** `tests/unit/api_statement_test.rs`,
`tests/corpus/api_oracle_test.rs`

#### Scenario: A statement recompiles after the schema moves

- GIVEN a prepared `SELECT` that has run once
- WHEN an unrelated table is created and the statement runs again
- THEN it returns the right row, its reprepare count is 1, and it does not
  recompile again while the schema holds still

**Tests:** `tests/unit/api_statement_test.rs::a_statement_recompiles_after_a_schema_change`

#### Scenario: A write prepared before an index maintains it afterwards

- GIVEN an `INSERT` prepared before any index existed
- WHEN two indexes are created and the same handle inserts more rows
- THEN the pinned oracle reports `integrity_check = ok` and finds every row
  through the index

  A unit test cannot make this claim: a stale program inserts the table row and
  skips the index, but a freshly-compiled read may table-scan and find it
  anyway. `integrity_check` is what detects a row present in the table with no
  matching index entry.

**Tests:** `tests/corpus/api_oracle_test.rs::a_prepared_write_after_create_index_keeps_the_file_valid`

#### Scenario: A statement whose table is dropped reports the failure

- GIVEN a prepared `SELECT` that has run once
- WHEN its table is dropped and the statement runs again
- THEN it fails rather than reusing the old program, and fails the same way on
  every subsequent attempt

**Tests:** `tests/unit/api_statement_test.rs::a_statement_whose_table_is_dropped_reports_the_failure`

## Not in this spec

- **A `sqlx` driver.** Out of tree (`sqlx-sqlite-rs`), so this crate's empty
  `[dependencies]` stays empty. Rationale and rejected alternatives: ADR-0041.
- **A C ABI, a rusqlite-shaped API, an `Arc` pager.** ADR-0041.
- **Async connections.** Blocking; `sqlx` drivers run blocking work on their own
  executor, and an async pager is a storage decision.
- **The PRAGMA catalogue.** plan.md V7 owns the list and its tiers; Requirement
  5 covers only what a pool sets and what durability requires.
- **Foreign-key enforcement** (V8), **`ATTACH`** (V10), and the two
  prerequisites above.
- **Non-POSIX platforms.** `UnixVfs` and the `src/sys/` carve-out are POSIX; a
  Windows VFS is spec 003's question.
