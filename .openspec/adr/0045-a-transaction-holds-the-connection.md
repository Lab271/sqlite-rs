# ADR-0045: A transaction holds the connection; other threads wait

**Date:** 2026-09-10
**Status:** Accepted

## Context

Spec 013 Requirement 4 says statements on a `Connection` are serialized, and
they are: the worker thread runs one request at a time. But a `Transaction`
is several statements, and `Connection` is `Clone` precisely so a pool or
several async tasks can hold it. Per-statement serialization says nothing
about what happens between them.

The first consumer to run two concurrent catalog commits found three
interleavings, reported against the facade:

1. Task B's `BEGIN` lands inside task A's open transaction and is refused
   with "cannot start a transaction within a transaction". Loud, and
   survivable.
2. Task B's statements land inside A's transaction and are committed with
   it — B's work becomes atomic with work it knows nothing about.
3. **An autocommit write from task C runs inside whichever transaction is
   open and is rolled back with it.** `execute` returned `Ok(1)`; the row is
   gone; nothing errored anywhere.

The third is the one that decides this ADR. It is a write that reported
success being silently discarded, which is the same failure class as two
connections on one file not locking against each other — except this one is
reachable through a single `Connection`, which is the object the API tells
consumers to share.

The consumer worked around it with a mutex around every call and said either
a fix or a documented caveat would do.

## Decision

`Connection::transaction` takes exclusion for the guard's lifetime. `Shared`
holds `Mutex<Option<TxnOwner>>` plus a `Condvar`; `transaction_with` claims
the slot *before* issuing `BEGIN`, and `Transaction`'s commit, rollback and
`Drop` release it and wake the waiters. Every request passes through
`Connection::send`, which is the single choke point, so the gate lives there.

Three arms, in order:

- The handle **is** the transaction — `Transaction` holds a `Connection`
  clone carrying the transaction's token. Proceeds.
- The handle is on the **thread that opened** the transaction. Proceeds.
  Holding a `Transaction` and continuing to use the original handle is what
  a single-threaded caller has always been able to do, and it matches
  SQLite, where any statement on a connection with an open transaction runs
  inside it. Blocking here would be a deadlock against oneself.
- Anyone else waits.

Re-entry from the thread that already holds the transaction returns
`Error::TransactionActive` rather than waiting: that is a nesting bug, and
`SAVEPOINT` is out of scope, so there is nothing legitimate to nest. Waiting
would hide the bug as a hang.

## Alternatives rejected

**Document it and leave the behaviour.** The consumer explicitly offered
this and it costs one sentence. Rejected because of interleaving 3: a caveat
does not make a lost write visible, and the guidance it would give — "wrap
your own mutex around it" — is exactly the code every consumer would then
write identically. If the correct use of a type is to always hold a lock
around it, the type should hold the lock.

**Hold a `MutexGuard` in `Transaction`.** The obvious shape, and not
expressible: `MutexGuard<'a, T>` carries a lifetime and `make check-mvl-limit`
forbids named lifetime parameters in `src/`. The same constraint that made
the worker thread the only expressible design (ADR-0041) applies here, which
is why this is a slot and a condvar rather than a guard.

**Make every statement claim the slot, including a raw `BEGIN` through
`execute`.** Would close the gap for consumers who write `BEGIN` as SQL
rather than calling `transaction()`. Rejected because nothing would release
it: a caller who issues `BEGIN` and then returns early leaves the connection
wedged for every other thread, with no `Drop` to recover. Stock SQLite offers
no such protection either. `execute("BEGIN")` therefore stays unguarded, and
that is a documented limit rather than an oversight.

## Consequences

- A `Transaction` leaked rather than dropped blocks every other thread on
  that connection, exactly as a leaked `MutexGuard` would. `Drop` is the
  release, so this requires actively forgetting the value.
- Re-entry from a *different thread of the same async task* cannot be
  distinguished from genuine contention and blocks. No API can see task
  identity; the consumer wraps blocking calls in `spawn_blocking`, so one
  task holds one thread for the duration, and the thread check covers it in
  practice.
- Mixing `transaction()` with a raw `execute("BEGIN")` on another thread is
  still unguarded, per the rejected alternative above.
- Throughput under contention drops to one transaction at a time per
  connection. That is what the consumer already achieves with its own mutex,
  and a connection is a single worker thread regardless.
