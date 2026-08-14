//! WAL-mode `-shm` reader-mark protocol: claims a `WAL_READ_LOCK` slot and
//! publishes the frame count a reader is pinned to (`aReadMark`), so a live
//! checkpointer backs off rather than backfilling/truncating WAL frames a
//! concurrent reader still depends on. Byte offsets and the wal-index
//! header layout verified against SQLite's own source (`os_unix.c`,
//! `wal.c`) by spike 005 (`tests/spike/005_locking_interop/findings.md`,
//! `src/wal_shm.rs`) — not re-derived here; experiment 4 there validated
//! this exact protocol against a live stock `sqlite3` checkpointer.
//!
//! wal-index (`-shm`) header layout (`WalIndexHdr` + `WalCkptInfo`,
//! `wal.c`):
//! ```text
//! offset  field
//! 0       WalIndexHdr copy 1 (48 bytes) — mxFrame at +16
//! 48      WalIndexHdr copy 2 (identical layout)
//! 96      WalCkptInfo.nBackfill (u32)
//! 100     WalCkptInfo.aReadMark[5] (u32 x5)
//! 120     WalCkptInfo.aLock[8]        <- UNIX_SHM_BASE
//! 128     WalCkptInfo.nBackfillAttempted (u32)
//! ```
#![allow(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::ptr;

use libc::{c_void, off_t, MAP_SHARED, PROT_READ, PROT_WRITE};

use super::lock::fcntl_lock;
use super::SharedLockGuard;

const MX_FRAME_OFFSET: isize = 16;
const READ_MARK_BASE_OFFSET: isize = 100;
#[cfg(test)]
const READ_MARK_UNUSED: u32 = 0xFFFF_FFFF;

/// SQLite's `UNIX_SHM_BASE` (`os_unix.c`): base of the `-shm` lock-byte
/// range.
const UNIX_SHM_BASE: off_t = 120;

/// `slot` ranges 1..=4 — slot 0 is reserved (always considered "in use" by
/// SQLite's own protocol) and is never claimed by a reader here, matching
/// spike 005 experiment 4.
fn wal_read_lock_byte(slot: usize) -> off_t {
    UNIX_SHM_BASE
        .saturating_add(3)
        .saturating_add(slot as off_t)
}

struct ShmMap {
    ptr: *mut u8,
    len: usize,
}

impl ShmMap {
    fn open(file: &File) -> io::Result<Self> {
        let len = file.metadata()?.len() as usize;
        let min_len = (READ_MARK_BASE_OFFSET as usize).saturating_add(20);
        if len < min_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "-shm file too short for a wal-index header",
            ));
        }
        // Safety: `file` is a valid, open fd for the lifetime of this
        // call; `MAP_SHARED` + a length validated above against the
        // file's own size keeps every offset this module reads/writes
        // in-bounds.
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                file.as_raw_fd(),
                0 as off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(ShmMap {
            ptr: ptr as *mut u8,
            len,
        })
    }

    /// Safety: `offset..offset+4` must be within `self.len`, checked by
    /// every call site below against the header's fixed, known-in-bounds
    /// offsets.
    unsafe fn read_u32(&self, offset: isize) -> u32 {
        ptr::read_unaligned(self.ptr.offset(offset) as *const u32)
    }

    unsafe fn write_u32(&self, offset: isize, value: u32) {
        ptr::write_unaligned(self.ptr.offset(offset) as *mut u32, value)
    }

    fn mx_frame(&self) -> u32 {
        // Safety: offset 16 + 4 bytes is within the 48-byte header copy,
        // itself within any `len` this type was constructed with.
        unsafe { self.read_u32(MX_FRAME_OFFSET) }
    }

    fn read_mark_offset(slot: usize) -> isize {
        READ_MARK_BASE_OFFSET.saturating_add((slot as isize).saturating_mul(4))
    }

    /// Test-only: reads back a published mark to verify `set_read_mark`.
    /// Production code never needs to read a mark it didn't just write —
    /// slot occupancy is determined by the lock, not the mark value (see
    /// `claim_wal_read_lock`'s doc comment).
    #[cfg(test)]
    fn read_mark(&self, slot: usize) -> u32 {
        // Safety: `slot` is always 1..=4 (test callers only), so this
        // offset stays within `aReadMark[5]` at 100..120.
        unsafe { self.read_u32(Self::read_mark_offset(slot)) }
    }

    fn set_read_mark(&self, slot: usize, value: u32) {
        // Safety: see `read_mark`.
        unsafe { self.write_u32(Self::read_mark_offset(slot), value) }
    }
}

impl Drop for ShmMap {
    fn drop(&mut self) {
        // Safety: `self.ptr`/`self.len` are exactly the values returned by
        // the `mmap` call that constructed this `ShmMap`, unmapped at most
        // once (`Drop` runs once).
        unsafe {
            libc::munmap(self.ptr as *mut c_void, self.len);
        }
    }
}

/// A claimed WAL reader-mark slot, released (lock dropped, mark left in
/// place) when this guard drops. Holds the `-shm` file open for its own
/// lifetime so the fd used to take the lock is never closed out from
/// under it — POSIX drops all `fcntl` record locks on `close()`.
#[derive(Debug)]
pub struct WalReadLock {
    file: File,
    slot: usize,
}

impl SharedLockGuard for WalReadLock {}

impl Drop for WalReadLock {
    fn drop(&mut self) {
        // Best-effort: `drop` can't propagate a failure, and there is
        // nothing more this crate can do about one anyway.
        let _ = fcntl_lock(
            self.file.as_raw_fd(),
            libc::F_UNLCK as libc::c_int,
            wal_read_lock_byte(self.slot),
            1,
        );
    }
}

/// Claims a WAL reader-mark slot on `shm_path` (SQLite's `<db>-shm`
/// companion file) at the WAL's current `mxFrame`, so a live checkpointer
/// backing off on this slot's lock never backfills past the frame count
/// this reader is relying on. Returns `Ok(None)` if `shm_path` doesn't
/// exist — no live WAL writer has ever opened this database, so there is
/// no checkpointer to coordinate with and nothing to lock. The existence
/// check is folded into the `open` call itself (rather than a separate
/// `try_exists`) so there's no TOCTOU window between checking and
/// opening for something else to replace the path with.
///
/// Tries each of the 4 reader slots (1..=4; slot 0 is reserved, matching
/// SQLite's own protocol) in order. A slot's lock, not its stale
/// `aReadMark` value, is what determines whether it's free — the mark of
/// a slot whose lock was already released is left in place (no reader
/// resets it on drop), so it can't be used to tell "free" from "held".
/// `mxFrame` is read fresh after each successful exclusive claim, not
/// once up front — a concurrent writer could otherwise advance it in the
/// gap between reading it and acquiring the lock, publishing a stale
/// mark. `Err` on the first non-contention `fcntl` failure, or if every
/// slot is genuinely contended (`EAGAIN`/`EACCES`).
///
/// Known residual risk (accepted, not fixed here): if the `-shm` file
/// shrinks after `ShmMap::open`'s length check (e.g. a concurrent
/// checkpoint truncates it), the mapping can extend past the file's
/// current backing store, and an access there raises `SIGBUS` — an
/// uncatchable process kill, not a Rust panic. This is inherent to any
/// mmap-based approach without a `SIGBUS` handler; sqlite-rs's threat
/// model here is a cooperating local `sqlite3` writer, not a sandboxed
/// adversary, so this is documented rather than mitigated.
pub(crate) fn claim_wal_read_lock(shm_path: &Path) -> io::Result<Option<WalReadLock>> {
    let file = match OpenOptions::new().read(true).write(true).open(shm_path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let shm = ShmMap::open(&file)?;

    let mut last_err = None;
    for slot in 1..=4usize {
        let byte = wal_read_lock_byte(slot);
        // Briefly exclusive, only long enough to publish this slot's mark
        // before downgrading to the SHARED lock held for the guard's
        // lifetime — matches SQLite's own claim sequence (spike 005 exp 4).
        match fcntl_lock(file.as_raw_fd(), libc::F_WRLCK as libc::c_int, byte, 1) {
            Ok(()) => {
                shm.set_read_mark(slot, shm.mx_frame());
                fcntl_lock(file.as_raw_fd(), libc::F_RDLCK as libc::c_int, byte, 1)?;
                return Ok(Some(WalReadLock { file, slot }));
            }
            Err(e) if matches!(e.raw_os_error(), Some(libc::EAGAIN) | Some(libc::EACCES)) => {
                last_err = Some(e);
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.unwrap_or_else(|| io::Error::other("no WAL read-lock slot available")))
}

/// Test-only: whether `slot` on `shm_path` is currently free, probed from
/// a forked child process via a non-blocking EXCLUSIVE lock attempt — for
/// tests outside this module (e.g. `src/pager/mod.rs`) that need to
/// observe reader-mark lock state without duplicating `fcntl` calls under
/// `unsafe_code`'s deny gate (`src/vfs/` is that gate's sole
/// qualified-subset exemption). A real second process is required: POSIX
/// record locks never conflict with a further request from the same
/// process (scoped to (process, inode)), so an in-process probe would
/// report "free" unconditionally.
#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test-only helper: a fork failure has no reasonable fallback"
)]
pub(crate) fn slot_is_free_test_only(shm_path: &Path, slot: usize) -> bool {
    // Safety: the child only performs async-signal-safe work (a raw
    // fcntl syscall, no allocation) before `_exit`.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => panic!("fork failed: {}", io::Error::last_os_error()),
        0 => {
            let ok = OpenOptions::new()
                .read(true)
                .write(true)
                .open(shm_path)
                .ok()
                .map(|f| {
                    fcntl_lock(
                        f.as_raw_fd(),
                        libc::F_WRLCK as libc::c_int,
                        wal_read_lock_byte(slot),
                        1,
                    )
                    .is_ok()
                })
                .unwrap_or(false);
            unsafe { libc::_exit(if ok { 0 } else { 1 }) };
        }
        pid => {
            let mut status: libc::c_int = 0;
            unsafe { libc::waitpid(pid, &mut status, 0) };
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Builds a minimal, valid-enough `-shm` file: a zeroed wal-index
    /// header with `mxFrame` set and every `aReadMark` slot unused.
    fn temp_shm(mx_frame: u32) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("sqlite-rs-shm-test-{}-{n}-shm", std::process::id()));
        let mut bytes = vec![0u8; 32768];
        bytes[MX_FRAME_OFFSET as usize..MX_FRAME_OFFSET as usize + 4]
            .copy_from_slice(&mx_frame.to_ne_bytes());
        for slot in 0..5 {
            let off = READ_MARK_BASE_OFFSET as usize + slot * 4;
            bytes[off..off + 4].copy_from_slice(&READ_MARK_UNUSED.to_ne_bytes());
        }
        let mut file = File::create(&path).unwrap();
        file.write_all(&bytes).unwrap();
        path
    }

    #[test]
    fn missing_shm_file_yields_no_lock() {
        let path = std::env::temp_dir().join("sqlite-rs-shm-test-missing-shm");
        assert!(claim_wal_read_lock(&path).unwrap().is_none());
    }

    #[test]
    fn claims_a_slot_and_publishes_mx_frame() {
        let path = temp_shm(42);

        let guard = claim_wal_read_lock(&path).unwrap().expect("shm exists");

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let shm = ShmMap::open(&file).unwrap();
        assert_eq!(shm.read_mark(guard.slot), 42);

        drop(guard);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn released_lock_can_be_reclaimed() {
        let path = temp_shm(7);

        let guard = claim_wal_read_lock(&path).unwrap().unwrap();
        let slot = guard.slot;
        drop(guard);

        // The released slot's lock is available again, so a later claim
        // that tries slots in the same order (1..=4) reclaims it, even
        // though its stale `aReadMark` value (left in place, not reset by
        // `drop`) doesn't reflect that.
        let guard2 = claim_wal_read_lock(&path).unwrap().unwrap();
        assert_eq!(guard2.slot, slot, "the now-free slot should be reclaimed");

        std::fs::remove_file(&path).unwrap();
    }

    /// A slot whose lock is held by another process must be skipped in
    /// favor of the next free one — POSIX record locks never conflict
    /// with a further request from the *same* process (they're scoped to
    /// (process, inode)), so a real second process is required to prove
    /// this, matching `src/vfs/lock.rs`'s `exclusive_lock_available`
    /// fork-based pattern.
    #[test]
    #[allow(
        clippy::panic,
        reason = "test-only helper: a fork failure has no reasonable fallback"
    )]
    fn contended_slot_is_skipped_for_the_next_free_one() {
        let path = temp_shm(5);
        let (parent_sock, child_sock) = std::os::unix::net::UnixDatagram::pair().unwrap();

        // Safety: the child only performs async-signal-safe work (raw
        // fcntl/socket syscalls, no allocation) before `_exit`.
        let pid = unsafe { libc::fork() };
        match pid {
            -1 => panic!("fork failed: {}", io::Error::last_os_error()),
            0 => {
                drop(parent_sock);
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap();
                fcntl_lock(
                    file.as_raw_fd(),
                    libc::F_RDLCK as libc::c_int,
                    wal_read_lock_byte(1),
                    1,
                )
                .expect("child claims slot 1");
                child_sock.send(b"locked").unwrap();
                let mut buf = [0u8; 4];
                let _ = child_sock.recv(&mut buf);
                unsafe { libc::_exit(0) };
            }
            pid => {
                drop(child_sock);
                let mut buf = [0u8; 16];
                parent_sock.recv(&mut buf).unwrap();

                let guard = claim_wal_read_lock(&path).unwrap().unwrap();
                assert_eq!(guard.slot, 2, "slot 1 is held, so slot 2 must be claimed");

                parent_sock.send(b"done").unwrap();
                let mut status: libc::c_int = 0;
                unsafe { libc::waitpid(pid, &mut status, 0) };
                drop(guard);
            }
        }

        std::fs::remove_file(&path).unwrap();
    }

    /// Forks one child per `slots`, each holding an `F_RDLCK` on its slot
    /// until signaled, and returns their pids plus the parent-side sockets
    /// to release them. Generalizes the single-slot fork in
    /// `contended_slot_is_skipped_for_the_next_free_one` to cover "every
    /// slot contended at once".
    #[allow(
        clippy::panic,
        reason = "test-only helper: a fork failure has no reasonable fallback"
    )]
    fn hold_slots_in_children(
        path: &std::path::Path,
        slots: &[usize],
    ) -> Vec<(libc::pid_t, std::os::unix::net::UnixDatagram)> {
        slots
            .iter()
            .map(|&slot| {
                let (parent_sock, child_sock) = std::os::unix::net::UnixDatagram::pair().unwrap();
                // Safety: the child only performs async-signal-safe work
                // (raw fcntl/socket syscalls, no allocation) before `_exit`.
                let pid = unsafe { libc::fork() };
                match pid {
                    -1 => panic!("fork failed: {}", io::Error::last_os_error()),
                    0 => {
                        drop(parent_sock);
                        let file = OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(path)
                            .unwrap();
                        fcntl_lock(
                            file.as_raw_fd(),
                            libc::F_RDLCK as libc::c_int,
                            wal_read_lock_byte(slot),
                            1,
                        )
                        .expect("child claims its slot");
                        child_sock.send(b"locked").unwrap();
                        let mut buf = [0u8; 4];
                        let _ = child_sock.recv(&mut buf);
                        unsafe { libc::_exit(0) };
                    }
                    pid => {
                        drop(child_sock);
                        let mut buf = [0u8; 16];
                        parent_sock.recv(&mut buf).unwrap();
                        (pid, parent_sock)
                    }
                }
            })
            .collect()
    }

    fn release_children(children: Vec<(libc::pid_t, std::os::unix::net::UnixDatagram)>) {
        for (pid, sock) in children {
            sock.send(b"done").unwrap();
            let mut status: libc::c_int = 0;
            unsafe { libc::waitpid(pid, &mut status, 0) };
        }
    }

    /// When every reader slot is genuinely contended, `claim_wal_read_lock`
    /// must return `Err`, not silently succeed or panic — the failure mode
    /// a `Pager::open` caller needs to distinguish from success.
    #[test]
    fn all_slots_contended_returns_err() {
        let path = temp_shm(9);
        let children = hold_slots_in_children(&path, &[1, 2, 3, 4]);

        let result = claim_wal_read_lock(&path);
        assert!(result.is_err(), "expected Err, got {result:?}");

        release_children(children);
        std::fs::remove_file(&path).unwrap();
    }

    /// A `-shm` file shorter than the wal-index header must be rejected,
    /// not read out-of-bounds or panic — a realistic input for a
    /// crash-truncated or half-written `-shm` file.
    #[test]
    fn truncated_shm_file_is_rejected() {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sqlite-rs-shm-test-{}-{n}-truncated-shm",
            std::process::id()
        ));
        std::fs::write(&path, vec![0u8; 32]).unwrap();

        let result = claim_wal_read_lock(&path);
        assert!(
            matches!(&result, Err(e) if e.kind() == io::ErrorKind::InvalidData),
            "expected InvalidData, got {result:?}"
        );

        std::fs::remove_file(&path).unwrap();
    }

    /// `UnixVfs::claim_wal_read_lock` (the trait-level entry point
    /// `Pager::open` actually calls) must surface lock contention as
    /// `VfsError::Locked`, not just this module's lower-level `io::Error`
    /// — the busy-detection contract applies to the WAL reader-mark path
    /// too, not only the main-db SHARED lock.
    #[test]
    fn unix_vfs_surfaces_locked_error_when_all_slots_contended() {
        use crate::vfs::{companion_path, UnixVfs, Vfs, VfsError};

        // `UnixVfs::claim_wal_read_lock` takes the *main db* path and
        // derives `<db>-shm` itself — so the shm file must live at
        // `db_path` + "-shm", not at a path already ending in "-shm".
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let db_path = std::env::temp_dir().join(format!(
            "sqlite-rs-shm-test-{}-{n}-unixvfs.db",
            std::process::id()
        ));
        let shm_path = companion_path(&db_path, "-shm");
        std::fs::rename(temp_shm(3), &shm_path).unwrap();

        let children = hold_slots_in_children(&shm_path, &[1, 2, 3, 4]);

        let result = UnixVfs.claim_wal_read_lock(&db_path);
        match result {
            Err(VfsError::Locked { .. }) => {}
            other => panic!("expected VfsError::Locked, got {other:?}"),
        }

        release_children(children);
        std::fs::remove_file(&shm_path).unwrap();
    }
}
