---
domain: embedding-api
version: 0.1.0
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

Decisions and rejected alternatives: ADR-0041.

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

`execute_transaction_step` returns rows and the new autocommit flag, so a
caller cannot distinguish an `UPDATE` that matched from one that did not. Every
optimistic-concurrency scheme is built on that distinction. *SQE* swaps a
table's metadata pointer with a conditional `UPDATE` and treats zero rows
affected as a lost race; without the count that becomes SELECT-then-UPDATE in a
transaction, sound only while the consumer guarantees a single writer, and every
consumer reinvents it.

**Implementation:** `src/api.rs::Connection::changes` (planned)

**Tests:** `tests/unit/api_changes_test.rs` (planned)

#### Scenario: A conditional update reports whether it matched

- GIVEN a row with `metadata_location = 'a'`
- WHEN `UPDATE t SET metadata_location = 'b' WHERE metadata_location = 'a'`
  runs, then the identical statement runs again
- THEN the first reports one row changed, the second reports zero, and the
  pinned oracle agrees with both

**Tests:** `tests/unit/api_changes_test.rs::conditional_update_reports_match` (planned)

#### Scenario: A SELECT does not clobber the count

- GIVEN a `DELETE` that removed two rows
- WHEN a `SELECT` returning no rows runs next
- THEN the count still reports two

**Tests:** `tests/unit/api_changes_test.rs::select_does_not_clobber_count` (planned)

### Requirement 2: Connection, Open or Create [MUST]

The API MUST open an existing database and, when asked, create a valid empty
one. Modes MUST distinguish read-only, read-write and read-write-create, and
MUST NOT create a file when create was not requested. Locks are spec 007's; the
handle MUST release them on drop, which `Pager` already does.

*SQE* opens its catalog as `sqlite://<path>?mode=rwc` and expects the file to
appear on first use; a first-run laptop has no `empty.db` to copy.

**Implementation:** `src/api.rs::Connection::open` (planned), plus `::open_with`

**Tests:** `tests/unit/api_connection_test.rs` (planned)

#### Scenario: Create produces a database the oracle reads

- GIVEN a path with no file at it
- WHEN opened `ReadWriteCreate`
- THEN a valid database exists and the pinned oracle reports an empty schema

**Tests:** `tests/unit/api_connection_test.rs::create_then_oracle_reads_empty_schema` (planned)

#### Scenario: Without create, nothing is written

- GIVEN a path with no file at it
- WHEN opened `ReadWrite`
- THEN it fails and no file exists afterwards

**Tests:** `tests/unit/api_connection_test.rs::readwrite_does_not_create` (planned)

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

**Implementation:** `src/api.rs::Statement` (planned), plus `::Row`

**Tests:** `tests/unit/api_statement_test.rs` (planned)

#### Scenario: One compile, many bindings

- GIVEN a prepared `SELECT name FROM t WHERE id = ?1`
- WHEN executed with 1, 2 and 3 bound
- THEN each returns that row's name and compilation happened once

**Tests:** `tests/unit/api_statement_test.rs::compile_once_bind_many` (planned)

#### Scenario: A named parameter is refused, not silently NULL

- GIVEN `SELECT * FROM t WHERE id = :id`
- WHEN prepared
- THEN preparation fails naming the unsupported form

**Tests:** `tests/unit/api_statement_test.rs::named_param_is_refused_at_prepare` (planned)

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

The thread MUST terminate on drop, and a request after it dies MUST error rather
than block. Coordination between connections in one process is spec 007's file
locks, as between processes; no shared cache, no global state.

**Implementation:** `src/api.rs::Connection` (planned)

**Tests:** `tests/unit/api_threading_test.rs` (planned)

#### Scenario: The handle is shared across threads

- GIVEN a connection opened on thread A
- WHEN its handle is cloned into several threads that each run a query
- THEN every query succeeds, a static assertion proves the handle is
  `Send + Sync`, and all engine access happened on the connection's thread

**Tests:** `tests/unit/api_threading_test.rs::handle_is_send_sync` (planned)

#### Scenario: The thread is released, and a dead engine errors

- GIVEN a loop that opens and drops connections
- WHEN it finishes
- THEN the thread count is unchanged, and a request on a dropped connection's
  handle errors instead of blocking

**Tests:** `tests/unit/api_threading_test.rs::worker_thread_joins_on_drop` (planned)

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
a distinct, documented busy variant, and a busy timeout MUST be settable per
connection.

**Implementation:** `src/api.rs::Transaction` (planned), plus `::Connection::pragma`
and `::ApiError`

**Tests:** `tests/unit/api_transaction_test.rs`, `tests/unit/api_durability_test.rs` (planned)

#### Scenario: Dropped transaction rolls back

- GIVEN an open transaction with one `INSERT` applied
- WHEN the handle drops without `commit()`
- THEN the row is absent and the pinned oracle agrees

**Tests:** `tests/unit/api_transaction_test.rs::drop_rolls_back` (planned)

#### Scenario: A committed transaction survives a hard kill

- GIVEN a transaction committed under `synchronous = FULL`
- WHEN the process is killed without unwinding and the database reopened
- THEN the rows are present and `integrity_check` passes under the oracle

**Tests:** `tests/unit/api_durability_test.rs::commit_survives_hard_kill` (planned)

#### Scenario: Busy is retryable and distinguishable

- GIVEN a second connection holding the WAL write lock
- WHEN a write is attempted
- THEN the error is the busy variant, `is_retryable()` is true, and a retry
  after release succeeds

**Tests:** `tests/unit/api_durability_test.rs::busy_is_retryable` (planned)

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

**Implementation:** `src/lib.rs` (planned) — module docs, plus `CHANGELOG.md`
policy and `tests/corpus/fixtures/consumers/sqe/`

**Tests:** `tests/unit/api_surface_test.rs`, `tests/corpus/consumer_sqe_test.rs` (planned)

#### Scenario: The facade needs no escape hatch

- GIVEN a consumer using only the items this spec defines
- WHEN it creates a database, prepares and binds a statement, reads rows, runs a
  transaction and reads the rows-affected count
- THEN it compiles without naming `pager`, `vdbe`, `codegen`, `dump` or `btree`

**Tests:** `tests/unit/api_surface_test.rs::facade_is_sufficient_alone` (planned)

#### Scenario: The consumer corpus matches the oracle

- GIVEN the catalog statement set above, run through this API and through the
  oracle
- THEN both produce identical rows in order, identical rows-affected counts, and
  identical files modulo documented header fields

**Tests:** `tests/corpus/consumer_sqe_test.rs::catalog_statements_match_oracle` (planned)

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

**Implementation:** `src/api.rs::Statement::next_row` (planned), or an `Iterator`
impl, built on `src/vdbe/exec.rs::Execution::next_row` rather than on
`execute_with_db` -- #682 found the ordering matters, because a facade
retrofitted onto the materializing entry point cannot be made incremental
afterwards

**Tests:** `tests/unit/api_streaming_test.rs` (planned)

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

**Tests:** `tests/unit/api_streaming_test.rs::partial_read_is_bounded` (planned)

#### Scenario: Abandoning a statement releases it

- GIVEN a statement read halfway
- WHEN it is dropped
- THEN its cursors are released and a subsequent write on the same connection
  proceeds

**Tests:** `tests/unit/api_streaming_test.rs::abandoned_statement_releases_cursors` (planned)

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
