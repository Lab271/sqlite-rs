# 0043 — The embedding API's failure surface: a flat error, both result codes, and autocommit-only busy retry

**Status:** Accepted · **Date:** 2026-09-09

## Context

Spec 013 Requirement 5 asks for three things that all land on the same type:
`VfsError::Locked` must surface as "a distinct, documented busy variant", a
busy timeout must be settable per connection, and a consumer must be able to
tell a UNIQUE violation from any other failure. ADR-0041 settled *where* the
API lives; it said nothing about what a failure looks like coming out of it.

Four choices had to be made, and each closes an alternative that is defensible
enough to be worth writing down.

**The error has to cross a thread.** Every failure travels back from the
connection's worker over a channel, so the error type must be `Send + Sync +
'static` unconditionally. An error that borrowed from engine state, or held an
`Rc`, could not be returned at all. That rules out the shape the sixteen engine
error enums have, several of which wrap layer errors by value.

**The result code is two numbers, not one.** `sqlite3_errcode()` returns the
primary code (19 for any constraint violation) and
`sqlite3_extended_errcode()` the extended one (2067 for UNIQUE specifically).
A caller asking "is this a constraint problem?" wants the first; one
distinguishing UNIQUE from NOT NULL wants the second. Picking one would make
the other unreachable, and the question of which a future `sqlx-sqlite-rs`
driver needs was genuinely open.

**A retried statement can be applied twice.** `Pager::flush`
(`src/pager.rs:524`) surfaces `VfsError::Locked` before any byte is journaled
and deliberately leaves `self.dirty` intact "so the caller can retry or roll
back". In autocommit, that dirty set is the statement's own work — already
applied in memory. Re-running the statement without discarding it first inserts
the row once per attempt. Measured before the rollback was added.

**A prepared statement can outlive its plan.** A compiled program addresses
tables by root page, and `DROP` returns that page to the freelist for a later
`CREATE` to reuse. A statement prepared before a schema change and run after it
can read a page belonging to a different table, with no error anywhere.

## Decision

**The error type is flat.** `api::Error`'s every payload is a `String`, an
`i32` or a `Copy` enum; layer errors arrive as already-formatted `Display`
text. It therefore derives `PartialEq` and is unconditionally `Send + Sync +
'static`. `#[non_exhaustive]`, so variants can be added without a breaking
change.

**Both result codes are exposed, under the names SQLite uses.**
`Error::sqlite_code()` returns the primary, `Error::extended_sqlite_code()` the
extended, and the primary is derived from the extended as the low byte — the
rule `sqlite3.h` encodes (`primary | (n<<8)`). `Error::is_retryable()` is true
for `Busy` and nothing else.

**`Busy` is classified structurally, never by message text.** The match is on
`ExecError::FlushFailed(PagerError::Vfs(VfsError::Locked { .. }))` and the
`DumpError` equivalents, not on a substring. Requirement 5 makes busy a
distinct *retryable* variant, so a classification that a reworded `Display`
could silently break is the wrong trade: every busy error would become
permanent and no test would fail.

**The busy timeout retries only in autocommit, and rolls back first.** In
autocommit the statement is the transaction, so `Pager::rollback` followed by
re-running it is a faithful retry of the whole unit. Inside an explicit
transaction it is not — the statement's mutations share the pending set with
every earlier statement's — so a busy there is reported immediately and is the
*transaction's* to retry. Stock SQLite behaves the same way with
`SQLITE_BUSY` at `COMMIT`. Backoff follows `sqliteDefaultBusyCallback`'s
ladder; the default timeout is zero, as SQLite's is.

**A stale prepared statement is recompiled, not rejected.** The connection
carries a schema generation, bumped whenever the catalog is invalidated; a
statement compiled against an older one is recompiled on next use. That is what
`sqlite3_prepare_v2` does on `SQLITE_SCHEMA`. If it no longer compiles at all,
the failure is reported and the handle stays registered, so the error is
repeatable rather than one-shot. The count is observable through
`Statement::reprepare_count`, mirroring `SQLITE_STMTSTATUS_REPREPARE`.

## Alternatives rejected

**An error that wraps its layer error and implements `source()`.** The
idiomatic Rust shape, and it would give callers the full chain. Rejected
because it cannot derive `PartialEq` (so tests substring-match messages
instead of asserting errors), and because making sixteen engine enums
`Send + Sync` to satisfy the channel is a large change to satisfy a facade.
The cost is real and small: the engine's enums barely implement `source()`
themselves, and the message they format is the diagnostic.

**One result code.** Simpler, and matches what most drivers expose. Rejected
because the two answer different questions and SQLite itself offers both; the
one-code version would have had to guess which, and the guess was open.

**Retrying inside a transaction too.** More uniform, and superficially more
useful. Rejected as unsound: it double-applies. A variant that rolled the whole
transaction back and asked the caller to replay is a real design, but it
requires the caller's statements, which the connection does not keep.

**Never retrying, and returning `Busy` for the caller to handle.** Honest, and
what the type already supports. Rejected because Requirement 5 makes a settable
timeout a MUST, and because every consumer would then write the same loop —
which is what the requirement exists to stop.

**Failing a stale statement with a schema error.** Safe, and what SQLite's
older `sqlite3_prepare()` did. Rejected because it pushes a retry loop onto
every caller for something the connection can do itself, and `prepare_v2`
exists precisely because that was the wrong default.

## Consequences

The error type is comparable in tests, which is why the API suites assert
`Error::ParamCount { expected: 2, found: 1 }` rather than matching on text. No
`source()` chain is available to consumers; if one is ever needed, it is an
additive change to a `#[non_exhaustive]` enum.

Busy handling is only exercisable against a *second process*, because two
connections in one process do not lock against each other at all (POSIX
`fcntl` is `(process, inode)`-scoped; stock SQLite closes this with
`unixInodeInfo`, and this crate has no equivalent). The corpus tests use the
pinned `sqlite3` as the lock holder, which also makes the claim stronger. The
in-process gap is tracked as a ratchet in
`tests/unit/api_durability_test.rs::in_process_connections_lock_against_each_other`
and is a pre-existing engine defect, not a consequence of this decision.

`Statement::reprepare_count` is public API that exists partly to make a claim
testable ("compilation happened once"). That is acceptable because it is
SQLite's own counter rather than an invention.
