---
domain: embedding-api
version: 0.1.0
status: draft
date: 2026-08-28
---

# 012 — Embedding API

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

Decisions and rejected alternatives: ADR-0033.

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

## What is missing today

Each line is checkable against the tree at 0.18.5.

1. **A rows-affected count.** Nothing in `src/vdbe/` reports how many rows an
   `INSERT`/`UPDATE`/`DELETE` changed. The only capability gap here.
2. **A facade.** No `Connection`, `Statement` or `Transaction`; the caller
   assembles pager, header, `Program` and a positional `Vec<Value>` by hand.
3. **A `Send + Sync` handle.** `Rc<dyn PageSource>` and `Rc<RefCell<Pager>>`
   are `!Send`, and ownership does not change that, so an async trait cannot
   hold the engine at all.
4. **A creation API.** `DatabaseHeader::new_empty_page1` is public
   (`src/header.rs:295`) but no API offers it, so `examples/README.md` records
   that the examples copy `fixtures/empty.db` instead.
5. **Named parameters.** `:name`, `@name`, `$name` reach the always-NULL stub
   ADR-0015 left in place, which a public facade would make reachable.
6. **A durability contract.** `Pager` syncs (`src/pager.rs:589,597,697,782`)
   but nothing states what is guaranteed, and `synchronous` has no handler.

## Prerequisites owned elsewhere

Two SQL-semantics gaps block a consumer and belong to V3/V7, not to this spec.
They are listed so nobody plans around the wrong gap.

- **`CREATE TABLE IF NOT EXISTS` is ignored after parsing.** `if_not_exists`
  appears only in `src/parser/grammar.rs` and `src/parser/printer.rs`.
- **A composite `PRIMARY KEY`/`UNIQUE` table constraint is not enforced and no
  `sqlite_autoindex_*` is created** (`src/codegen/stmt/insert.rs` documents
  this). Two consequences: duplicates are accepted where stock SQLite raises a
  constraint error, and the schema diverges from what the oracle writes for the
  same DDL, which Requirement 6's acceptance check will surface.

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

**Implementation:** `src/api.rs::Connection::open`, `::open_with` (planned)

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

**Implementation:** `src/api.rs::Statement`, `::Row` (planned)

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
the connection -- this requirement specifies the second. `sqlx`'s own SQLite
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
least to distinguish FULL from OFF. Sync points exist
(`src/pager.rs:589,597,697,782,811,845`); what is missing is a documented
guarantee and any way to trade it. A consumer storing pointers to data it cannot
otherwise find needs that in writing: *SQE*'s file holds the metadata pointer
for every table in a warehouse, so a commit returning before it is durable turns
a power failure into tables that exist on object storage and are unreachable.
Where a PRAGMA is accepted without being honored, record it as a divergence
under ADR-0004.

Retryable errors belong here too: spec 007's `VfsError::Locked` MUST surface as
a distinct, documented busy variant, and a busy timeout MUST be settable per
connection.

**Implementation:** `src/api.rs::Transaction`, `::Connection::pragma`, `::ApiError` (planned)

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

**Implementation:** `src/lib.rs` module docs, `CHANGELOG.md` policy,
`tests/corpus/fixtures/consumers/sqe/` (planned)

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

## Not in this spec

- **A `sqlx` driver.** Out of tree (`sqlx-sqlite-rs`), so this crate's empty
  `[dependencies]` stays empty. Rationale and rejected alternatives: ADR-0033.
- **A C ABI, a rusqlite-shaped API, an `Arc` pager.** ADR-0033.
- **Async connections.** Blocking; `sqlx` drivers run blocking work on their own
  executor, and an async pager is a storage decision.
- **The PRAGMA catalogue.** plan.md V7 owns the list and its tiers; Requirement
  5 covers only what a pool sets and what durability requires.
- **Foreign-key enforcement** (V8), **`ATTACH`** (V10), and the two
  prerequisites above.
- **Non-POSIX platforms.** `UnixVfs` and the `src/sys/` carve-out are POSIX; a
  Windows VFS is spec 003's question.
