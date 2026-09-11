// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Process-wide registry keyed by a file's `(device, inode)`, mediating
//! same-process lock requests *before* any real `fcntl` call is made
//! (#706). Reference: stock `sqlite3`'s `unixInodeInfo` (`os_unix.c`).
//!
//! POSIX `fcntl(F_SETLK)` record locks are scoped to `(process, inode)`,
//! not to a file descriptor or a Rust value: two independently-opened
//! handles on the same file **in the same process** never conflict with
//! each other at the OS level — the kernel considers them the same lock
//! holder. `src/vfs/lock.rs`'s own doc comments already note this (`file()`,
//! line ~96); what was missing is the thing stock SQLite builds on top of
//! it, so two `Connection`s opened on one file in one process actually
//! serialize the way two processes already do, instead of one silently
//! discarding the other's write.
//!
//! [`SharedInodeLock`] is that layer: exactly one real, fcntl-backed
//! [`FileLockState`] per `(device, inode)`, process-wide — every
//! `UnixVfsFile` opened on the same underlying file shares it (via the
//! registry below) instead of each minting its own. On top of that single
//! real lock ladder, it tracks the in-process bookkeeping (`shared_holders`,
//! `write_holder`) that lets each *step* of the ladder refuse a transition
//! that would otherwise silently succeed against the OS but conflicts with
//! another handle in this same process — this is the layer stock SQLite's
//! `unixInodeInfo` provides and this crate didn't.
//!
//! [`ExclusiveGate`] is the same idea in miniature, for `src/vfs/shm.rs`'s
//! single-byte `WAL_WRITE_LOCK`/`WAL_CKPT_LOCK` (#491): those already share
//! one `-shm` fd per path (`open_shm_shared`), which fixes the "closing any
//! fd drops every lock" hazard, but nothing stopped two in-process holders
//! from both winning the same non-blocking `fcntl` byte lock — this gate is
//! that check.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::Hash;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use super::lock::{FileLockState, LockLevel};
use crate::sys::fcntl::EAGAIN;

type InodeKey = (u64, u64);

/// `Weak` entries so an inode with no more live handles is dropped
/// (releasing its real fcntl locks via `FileLockState`'s own `Drop`)
/// instead of pinned in this registry forever — the "entry outlives
/// handles, removed when the last one goes" lifecycle the ticket calls
/// for. Entries are pruned opportunistically in [`claim`], the only
/// place that touches the map.
static REGISTRY: OnceLock<Mutex<HashMap<InodeKey, Weak<Mutex<SharedInodeLock>>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<InodeKey, Weak<Mutex<SharedInodeLock>>>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The process-wide lock-arbitration state for one `(device, inode)`.
pub(crate) struct SharedInodeLock {
    real: FileLockState,
    /// Whether the fd currently behind `real` was opened with write
    /// access. `fcntl(F_SETLK, F_WRLCK, ...)` — every step past `Shared`
    /// on the ladder — fails `EBADF` on an fd opened read-only, so a
    /// registry entry first created by a read-only opener (e.g. a header
    /// peek) must be upgraded before a later writer shares it, not just
    /// reused as-is (see [`claim`]'s own doc comment).
    writable: bool,
    /// Count of in-process handles currently holding at least `Shared`
    /// (the write-holder, if any, is included in this count — RESERVED is
    /// held *alongside* SHARED, per the ladder's own doc comments).
    shared_holders: u32,
    /// Whether some in-process handle already holds `Reserved` or
    /// higher — at most one may, mirroring the real ladder's own
    /// single-writer invariant, just enforced against this process's
    /// other handles too, which raw `fcntl` never does.
    write_holder: bool,
}

impl SharedInodeLock {
    fn new(file: File, writable: bool) -> Self {
        SharedInodeLock {
            real: FileLockState::new(file),
            writable,
            shared_holders: 0,
            write_holder: false,
        }
    }

    /// Replaces the fd behind `real` with a freshly-opened, write-capable
    /// one — used by [`claim`] when a write-needing caller finds an
    /// existing entry that was only ever opened read-only. Only valid
    /// while `real` is at `Unlocked` (a read-only opener never calls
    /// `lock_shared`, so this holds in practice; `claim` doesn't call
    /// this otherwise).
    fn upgrade_to_writable(&mut self, file: File) {
        self.real = FileLockState::new(file);
        self.writable = true;
    }

    /// Runs `f` with the shared, real fd behind this inode — never a
    /// second, independently-opened fd to the same path (see this
    /// module's doc comment for why that would be a correctness hazard,
    /// not just a wasted syscall). Closure-based rather than returning a
    /// borrowed reference: every `FileExt` method this crate calls
    /// (`read_at`/`write_at`/`sync_data`/`set_len` via `Metadata`) takes
    /// `&File`, so there's never a need to hold a borrow across more
    /// than one call.
    pub(crate) fn with_file<R>(&self, f: impl FnOnce(&File) -> R) -> R {
        f(self.real.file())
    }

    /// Whether some *other* in-process or cross-process holder has
    /// RESERVED — `false` when queried by a handle that is itself the
    /// current in-process write-holder (matching
    /// `sqlite3OsCheckReservedLock`'s "not my own lock" semantics).
    pub(crate) fn check_reserved(&self, caller_holds_write: bool) -> io::Result<bool> {
        if caller_holds_write {
            return Ok(false);
        }
        Ok(self.write_holder || self.real.check_reserved()?)
    }

    /// Performs exactly one ladder step (`from` -> `to`, adjacent rungs
    /// only — callers step one level at a time, same as
    /// `FileLockState::set_level`'s own loop), applying in-process
    /// arbitration on the rungs where a same-process `fcntl` call would
    /// not itself detect a conflict.
    fn step(&mut self, from: LockLevel, to: LockLevel) -> io::Result<()> {
        use LockLevel::*;
        match (from, to) {
            (Unlocked, Shared) => {
                // A new reader must not start once some in-process
                // writer is mid-ladder (PENDING or EXCLUSIVE) — the same
                // rule the real ladder already enforces against *other*
                // processes (`step_up`'s own PENDING_BYTE probe).
                if self.real.lock_state() >= Pending {
                    return Err(would_block());
                }
                if self.shared_holders == 0 {
                    self.real.set_level(Shared)?;
                }
                self.shared_holders = self.shared_holders.saturating_add(1);
            }
            (Shared, Reserved) => {
                if self.write_holder {
                    return Err(would_block());
                }
                self.real.set_level(Reserved)?;
                self.write_holder = true;
            }
            (Reserved, Pending) | (Exclusive, Pending) => {
                self.real.set_level(Pending)?;
            }
            (Pending, Exclusive) => {
                // EXCLUSIVE needs the whole SHARED range to itself — a
                // same-process fcntl call would not see another
                // in-process handle still holding SHARED, so that has to
                // be checked here.
                if self.shared_holders > 1 {
                    return Err(would_block());
                }
                self.real.set_level(Exclusive)?;
            }
            (Pending, Reserved) => {
                self.real.set_level(Reserved)?;
            }
            (Reserved, Shared) => {
                self.real.set_level(Shared)?;
                self.write_holder = false;
            }
            (Shared, Unlocked) => {
                self.shared_holders = self.shared_holders.saturating_sub(1);
                if self.shared_holders == 0 {
                    self.real.set_level(Unlocked)?;
                }
            }
            (a, b) if a == b => {}
            (from, to) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "non-adjacent lock transition {from:?} -> {to:?}: callers must \
                         step one ladder rung at a time"
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn would_block() -> io::Error {
    io::Error::from_raw_os_error(EAGAIN)
}

fn lock_registry_or_recover(
    registry: &Mutex<HashMap<InodeKey, Weak<Mutex<SharedInodeLock>>>>,
) -> std::sync::MutexGuard<'_, HashMap<InodeKey, Weak<Mutex<SharedInodeLock>>>> {
    registry.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Returns the shared, process-wide [`SharedInodeLock`] for `path`'s
/// underlying `(device, inode)`, opening `path` via `open` only when no
/// existing entry already covers that inode.
///
/// The whole lookup-open-insert sequence runs under the registry's single
/// `Mutex`, matching stock SQLite's own `unixOpen`/`findInodeInfo` (both
/// run under `unixEnterMutex`) — not for throughput, but for correctness:
/// `open` must never run for a path that already resolves to a tracked
/// inode. An extra, unused fd opened (and then dropped) after a hit would
/// itself close every lock this process holds on that inode the moment
/// it drops (the same POSIX scoping this whole module exists to work
/// around), so the redundant open has to be prevented outright, not
/// cleaned up after the fact.
pub(crate) fn claim(
    path: &Path,
    needs_write: bool,
    open: impl FnOnce() -> io::Result<File>,
) -> io::Result<Arc<Mutex<SharedInodeLock>>> {
    let mut map = lock_registry_or_recover(registry());
    map.retain(|_, weak| weak.strong_count() > 0);

    if let Ok(meta) = std::fs::metadata(path) {
        let key = (meta.dev(), meta.ino());
        if let Some(existing) = map.get(&key).and_then(Weak::upgrade) {
            if needs_write {
                let mut inner = existing.lock().unwrap_or_else(PoisonError::into_inner);
                if !inner.writable {
                    // A read-only opener (e.g. a header peek before the
                    // real `Pager::open`) claimed this inode first. A
                    // read-only fd never calls `lock_shared`, so this
                    // entry has taken no real lock yet — safe to reopen
                    // write-capable and swap in place, rather than
                    // handing this writer an fd whose every `F_WRLCK`
                    // step would fail `EBADF`.
                    inner.upgrade_to_writable(open()?);
                }
                drop(inner);
            }
            return Ok(existing);
        }
    }

    let file = open()?;
    let meta = file.metadata()?;
    let key = (meta.dev(), meta.ino());
    let entry = Arc::new(Mutex::new(SharedInodeLock::new(file, needs_write)));
    map.insert(key, Arc::downgrade(&entry));
    Ok(entry)
}

/// One in-process handle's view of its own place on the ladder, plus the
/// shared, process-wide state every handle on this inode steps through.
/// Mirrors [`FileLockState`]'s own `level` field, just resolved through
/// [`SharedInodeLock::step`] instead of calling `fcntl` directly.
pub(crate) struct InodeLockHandle {
    shared: Arc<Mutex<SharedInodeLock>>,
    level: LockLevel,
}

fn lock_shared_inode(
    shared: &Arc<Mutex<SharedInodeLock>>,
) -> std::sync::MutexGuard<'_, SharedInodeLock> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

impl InodeLockHandle {
    pub(crate) fn new(shared: Arc<Mutex<SharedInodeLock>>) -> Self {
        InodeLockHandle {
            shared,
            level: LockLevel::Unlocked,
        }
    }

    pub(crate) fn check_reserved(&self) -> io::Result<bool> {
        lock_shared_inode(&self.shared).check_reserved(self.level >= LockLevel::Reserved)
    }

    pub(crate) fn set_level(&mut self, target: LockLevel) -> io::Result<()> {
        while self.level < target {
            let next = successor(self.level);
            lock_shared_inode(&self.shared).step(self.level, next)?;
            self.level = next;
        }
        while self.level > target {
            let prev = predecessor(self.level);
            lock_shared_inode(&self.shared).step(self.level, prev)?;
            self.level = prev;
        }
        Ok(())
    }
}

impl Drop for InodeLockHandle {
    fn drop(&mut self) {
        // Best-effort, matching `FileLockState`'s own `Drop`: nothing
        // more can be done about a failure here.
        self.set_level(LockLevel::Unlocked).ok();
    }
}

fn successor(level: LockLevel) -> LockLevel {
    use LockLevel::*;
    match level {
        Unlocked => Shared,
        Shared => Reserved,
        Reserved => Pending,
        Pending | Exclusive => Exclusive,
    }
}

fn predecessor(level: LockLevel) -> LockLevel {
    use LockLevel::*;
    match level {
        Exclusive => Pending,
        Pending => Reserved,
        Reserved => Shared,
        Shared | Unlocked => Unlocked,
    }
}

/// A process-wide "at most one in-process holder at a time" gate keyed by
/// an arbitrary `Eq + Hash` identity — the same in-process arbitration
/// [`SharedInodeLock`] gives the main db file's multi-level ladder,
/// shrunk to the single-exclusive-holder case `src/vfs/shm.rs`'s
/// `WAL_WRITE_LOCK`/`WAL_CKPT_LOCK` need (#491): those already share one
/// `-shm` fd per path, but nothing stopped a second in-process handle
/// from also winning the same non-blocking `fcntl` byte lock.
pub(crate) struct ExclusiveGate<K> {
    held: Mutex<HashSet<K>>,
}

impl<K: Eq + Hash> ExclusiveGate<K> {
    pub(crate) fn new() -> Self {
        ExclusiveGate {
            held: Mutex::new(HashSet::new()),
        }
    }

    /// Claims `key` if no in-process holder already has it. Returns
    /// `false` (refused) rather than blocking, matching every other lock
    /// primitive in this crate (`F_SETLK`, never `F_SETLKW`).
    pub(crate) fn acquire(&self, key: K) -> bool {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key)
    }

    pub(crate) fn release(&self, key: &K) {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
    }
}
