# 0047 — A process-wide `(device, inode)` registry arbitrates in-process locks before `fcntl`

**Status:** Accepted · **Date:** 2026-09-11

## Context

Two `Connection`s (in this crate's current, pre-#705 form: two independent
`Pager`s built directly over `UnixVfs`) opened on the same file **in the
same process** did not lock against each other (#706). Measured: A takes
`BEGIN IMMEDIATE` and inserts a row; B inserts a second row and gets `Ok`;
A commits; B's row is gone, silently, and `PRAGMA integrity_check` still
reports `ok`.

POSIX `fcntl(F_SETLK)` record locks are scoped to `(process, inode)`, not
to a file descriptor or a Rust value: two fds opened independently by the
same process on the same inode never conflict with each other at the OS
level — the kernel treats them as the same lock holder. `src/vfs/lock.rs`
already documented this (`FileLockState::file`'s doc comment, ~line 96)
and `check_reserved_lock`'s own doc comment said outright it "cannot see
another handle in the same process." What was missing is the layer stock
SQLite builds on top of that fact: `unixInodeInfo` (`os_unix.c`), a
process-wide registry keyed by `(device, inode)` that arbitrates between
a process's own handles *before* any `fcntl` call is made, so that two
handles on one inode serialize the way two processes already correctly
do.

#491 asked the adjacent question for the three WAL `-shm` guards
(`WalWriteLock`/`WalCheckpointLock`/`WalReadLock`) and was closed
COMPLETED without an answer recorded. Investigating it here: `src/vfs/shm.rs`
already shares one `-shm` fd per path via `open_shm_shared`'s `Weak<File>`
registry, which fixes the "closing any fd drops every lock this process
holds on that inode" hazard — but nothing stopped two in-process holders
from both winning the same non-blocking `F_WRLCK` byte lock, since that
hazard and *this* one (#706's) are different bugs that happen to share a
root cause. #491 is answered here, not just asserted: the gap was real
and reachable, and is now closed for `WAL_WRITE_LOCK`/`WAL_CKPT_LOCK`
(`WalReadLock`'s per-slot in-process race is a known, non-data-lossy
residual — see Consequences).

## Decision

`src/vfs/inode_registry.rs`: a process-wide `Mutex<HashMap<(dev, ino),
Weak<Mutex<SharedInodeLock>>>>`. `SharedInodeLock` wraps exactly one real,
fcntl-backed `FileLockState` per inode (shared, not one per `UnixVfsFile`
the way it was before), plus in-process bookkeeping — `shared_holders`
(a count) and `write_holder` (a bool) — that gates each ladder transition
*before* the real `fcntl` call:

- `Unlocked -> Shared`: refused if any in-process handle already holds
  `Pending`/`Exclusive` (mirrors the real ladder's own PENDING_BYTE
  probe, just against this process's own handles too).
- `Shared -> Reserved`: refused if `write_holder` is already `true`.
- `Pending -> Exclusive`: refused if `shared_holders > 1` (some other
  in-process handle still holds `Shared`).

`claim(path, needs_write, open)` looks up the entry by `path`'s
`(device, inode)` (via `std::fs::metadata`, not by opening first — see
Consequences) and reuses it if found, only calling `open` when no entry
exists; the whole lookup-open-insert sequence runs under the registry's
one `Mutex`, matching stock SQLite's `unixEnterMutex`-guarded
`findInodeInfo`. `UnixVfsFile`/`UnixLockGuard` (`src/vfs/unix.rs`) now
hold an `Arc<Mutex<SharedInodeLock>>` instead of each minting its own
`Rc<RefCell<FileLockState>>`. `ExclusiveGate<K>`, a smaller sibling type
in the same module, gives `src/vfs/shm.rs`'s `WAL_WRITE_LOCK`/
`WAL_CKPT_LOCK` the same in-process arbitration in miniature (an
at-most-one-in-process-holder set keyed by `-shm` path), reusing
`open_shm_shared`'s existing fd-sharing rather than replacing it.

## Alternatives rejected

- **Leave `UnixVfsFile` per-call, add a global "is this path open
  elsewhere" flag only at `Pager::open` time.** Doesn't compose: the
  actual conflict is per-ladder-*level*, not per-open — a `BEGIN
  IMMEDIATE` on handle A must block handle B's escalation specifically,
  while B's own `Shared` read alongside A's `Reserved` must still be
  fine (RESERVED is held *alongside* SHARED). A single flag can't express
  that; the ladder-aware `SharedInodeLock` can.
- **Key the registry by canonicalized path instead of `(device, inode)`.**
  This is exactly the workaround the issue calls out the SQE consumer
  already having to build on their own — hardlinks/bind mounts/`..`
  segments make two different path strings resolve to one inode (or
  vice versa across mount namespaces), which `(device, inode)` gets
  right by construction and path canonicalization does not.
- **Block (real `F_SETLKW`) instead of refusing.** Every other lock
  primitive in this crate is non-blocking `F_SETLK`, mapped to
  `VfsError::Locked`/`SQLITE_BUSY`-style retry at the caller. Blocking
  here would be the only exception, and would risk an in-process
  deadlock (two handles on the same thread, one waiting on the other)
  that a non-blocking refusal can't cause.
- **Reuse the cached fd unconditionally regardless of open mode.** Tried
  first, and wrong: a registry entry created by a read-only opener (e.g.
  `dump.rs`'s header peek before `Pager::open`) handed a later writer an
  fd with no write access, and every `F_WRLCK` step on it failed `EBADF`
  — caught by `tests/tiers/tier0.rs::t0_hot_journal_recovers_committed_state`
  once real fd-sharing was wired in. Fixed by tracking `writable` and
  reopening in place (`SharedInodeLock::upgrade_to_writable`) the one
  time a write-needing caller finds a read-only entry — safe because a
  read-only opener never calls `lock_shared`, so the entry it created is
  always still `Unlocked` when this happens.

## Consequences

- `claim`'s lookup-open-insert runs under one global `Mutex` for the
  *whole* operation, including the `open()` syscall — a deliberate
  serialization of every `Vfs::open_*` call process-wide (matching
  `unixEnterMutex`'s own scope), not a fast path. Opens are rare relative
  to reads/writes, so this is judged acceptable; if profiling ever shows
  otherwise, narrowing the critical section is a follow-up, not a
  redesign.
- `WalReadLock`'s per-slot claim (`src/vfs/shm.rs::claim_wal_read_lock`)
  is **not** routed through an in-process arbiter in this change: two
  in-process readers can still both win the same reader-mark slot. This
  is deliberately out of scope here — the harm class #706 is about is a
  writer's success being silently discarded, which this residual does
  not cause (worst case: a checkpoint bounds itself more conservatively
  than necessary against a slot two readers happen to share). Follow-up
  ticket if it needs closing properly.
- `tests/corpus/in_process_lock_registry_test.rs` proves the issue's
  exact scenario, a cross-process regression guard, a handle-close/
  reopen lifecycle check, and a pool-of-N-handles progress check, all
  against real `Pager`s/`BEGIN IMMEDIATE` — no mocked locks.

## Related

Refs: #706, #491, #412
