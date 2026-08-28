# 0033 — The embedding API owns the connection; the `sqlx` driver stays out of tree

**Status:** Proposed · **Date:** 2026-08-28

## Context

`examples/README.md`: "this crate exposes its parser/codegen/VM pipeline
directly rather than an ergonomic `Connection`/`prepare`/`bind` wrapper, so each
example wires those pieces together the same way the `sqlite-rs` CLI binary
does."

The pieces are built. `Vm::bind_params` and the `Variable` opcode (spec 009,
ADR-0015), `compile_statement`, the autocommit state `execute_transaction_step`
threads, and `Pager`'s file locks (spec 007). Creating a database shows the
shape of what is left: the primitive is public
(`DatabaseHeader::new_empty_page1`) and the CLI calls it, but no API offers it.
Missing, then: a facade, a `Send + Sync` boundary, and one counter.

The counter is the only genuine capability gap. Nothing in `src/vdbe/` reports
rows changed, so a caller cannot tell a conditional `UPDATE` that matched from
one that did not, and optimistic concurrency is built on exactly that. The
driving consumer (SQE, which stores Iceberg catalog pointers in SQLite) is
working around it with SELECT-then-UPDATE in a transaction, sound only while a
single writer is guaranteed.

Spec 012 defines the surface and makes the counter its Requirement 1. This ADR
records what that closes.

## Decision

**A native `Connection`/`Statement`/`Transaction` API in `src/api.rs`** with a
rows-affected count, open-or-create modes, positional `?` binding, explicit
transactions, a stated durability contract, and a retryable busy error distinct
from fatal ones. Named parameters are rejected at prepare time rather than
reaching execution as ADR-0015's always-NULL stub, which a public facade would
otherwise make reachable and unattributable.

**A `Send + Sync` handle over a connection-owned worker thread.** `Rc` is not
`Send` and ownership does not change that, so making the connection type itself
`Send` requires an `Arc` refactor of `Pager`/`PageSource`. The connection
instead creates its `Rc` graph on a thread it owns and never lets it leave.
`sqlx`'s own SQLite driver does this for a C `sqlite3*`
(`sqlx-sqlite-0.9.0/src/connection/worker.rs`), which is evidence the shape is
the standard answer rather than a concession.

**The `sqlx` driver lives in a separate crate** (`sqlx-sqlite-rs`) and is a
`SHOULD`, not a `MUST`. This crate's `[dependencies]` stays empty. SQE showed
the driver is not the only path by implementing its catalog trait directly over
the public internals; the driver remains right for consumers who will not write
one.

## Alternatives rejected

**A C ABI shim** exporting `sqlite3_open`/`sqlite3_prepare_v2`/`sqlite3_step`
so `libsqlite3-sys` links here. Every existing consumer in every language would
work unchanged. Rejected: it reintroduces the `unsafe` boundary the crate exists
to remove, needs a carve-out larger than `src/sys/`, returns the error surface
to null-pointer semantics, and forces the C threading contract on a design that
gets to choose one. Defensible later, on top of spec 012; a bad substitute for
it.

**Cloning rusqlite's API.** Rejected: `.openspec/README.md` already commits to
"inspired by rusqlite but not a wrapper", and rusqlite's shape is dictated by C
ownership (`Connection` is `!Sync` because `sqlite3*` is, statements borrow
through a `RefCell` cache, `close` hands the connection back on failure).
Copying it imports constraints this crate does not have.

**Making `Pager` `Arc`/`Mutex`.** Rejected on the grounds ADR-0013 and ADR-0017
already established: one `Vm` shares a page source across N cursors via cheap
`Rc` clones, so `Arc` taxes the Tier 0 read path to serve a requirement that
lives at the connection boundary.

**Putting the `sqlx` driver in this repository**, feature-gated. Rejected: an
empty `[dependencies]` table is one of this crate's two headline properties, a
feature-gated dependency still appears in `Cargo.lock`, the SBOM and
`cargo deny`, and it would couple this crate's cadence to `sqlx`'s. A separate
crate can track `sqlx` 0.9, 0.10 and 1.0 while this API stays still.

**Async connections.** Rejected for now, not on principle: `sqlx` drivers run
blocking work on their own executor, so async buys the driving consumer nothing,
and an async pager is a storage decision spec 012 must not pre-empt.

## Consequences

- Spec 012 needs its own value block. It sits outside the V1--V12 ladder (those
  deliver SQL surface, it delivers consumability) and is a prerequisite for V7's
  stated demo, so it belongs before V8. One minor per phase (ADR-0006) implies
  its own minor.
- `foreign_keys = ON` will be accepted before it is enforced (V8), because
  `sqlx` issues it unconditionally. That divergence needs its own ADR under
  ADR-0004, not a silent no-op.
- The rows-affected count is the one item a consumer cannot work around
  cleanly. Shipping the facade without it leaves every compare-and-swap consumer
  reinventing a single-writer workaround, and the ones who get it wrong lose
  writes silently.
- Two SQL-semantics prerequisites surface alongside this work and belong to
  V3/V7: `CREATE TABLE IF NOT EXISTS` is ignored after parsing, and a composite
  `PRIMARY KEY`/`UNIQUE` table constraint is neither enforced nor backed by an
  auto-created `sqlite_autoindex_*`. The second also affects this crate's own
  byte-compatibility claim, which makes it the highest-value item in the set.
- Consumer statement sets become fixture families under spec 004's harness
  (spec 012/Req-6), so "an application can use this" is measured rather than
  asserted.
