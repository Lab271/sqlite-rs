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
//!
//! No longer `mmap`s the `-shm` file (#66): every field access here is a
//! `pread`/`pwrite` (`std::os::unix::fs::FileExt`) at these same fixed
//! offsets. Coherence with a concurrent `sqlite3` process's own `MAP_SHARED`
//! mapping of this file relies on the OS's unified page cache keeping
//! buffered file I/O and `mmap`'d access to the same file coherent — true
//! on Linux and macOS, sqlite-rs's supported platforms. A bonus of this
//! approach over `mmap`: a `-shm` file truncated out from under a reader
//! now yields a structured `Err` from the read, not an uncatchable
//! `SIGBUS`.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use nix::libc::{self, off_t};

use super::lock::fcntl_lock;
use super::SharedLockGuard;

const MX_FRAME_OFFSET: u64 = 16;
const READ_MARK_BASE_OFFSET: u64 = 100;
#[cfg(test)]
const READ_MARK_UNUSED: u32 = 0xFFFF_FFFF;

/// Minimum `-shm` file length for a valid wal-index header: through the
/// end of `aReadMark[5]` at offset 100..120.
const MIN_SHM_LEN: u64 = READ_MARK_BASE_OFFSET + 20;

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

fn read_mark_offset(slot: usize) -> u64 {
    READ_MARK_BASE_OFFSET.saturating_add((slot as u64).saturating_mul(4))
}

fn read_u32_at(file: &File, offset: u64) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    file.read_exact_at(&mut buf, offset)?;
    Ok(u32::from_ne_bytes(buf))
}

fn write_u32_at(file: &File, offset: u64, value: u32) -> io::Result<()> {
    file.write_all_at(&value.to_ne_bytes(), offset)
}

fn mx_frame(file: &File) -> io::Result<u32> {
    read_u32_at(file, MX_FRAME_OFFSET)
}

fn set_read_mark(file: &File, slot: usize, value: u32) -> io::Result<()> {
    write_u32_at(file, read_mark_offset(slot), value)
}

/// Test-only: reads back a published mark to verify `set_read_mark`.
/// Production code never needs to read a mark it didn't just write — slot
/// occupancy is determined by the lock, not the mark value (see
/// `claim_wal_read_lock`'s doc comment).
#[cfg(test)]
fn read_mark(file: &File, slot: usize) -> io::Result<u32> {
    read_u32_at(file, read_mark_offset(slot))
}

/// Validates that `file` is at least long enough to hold a full wal-index
/// header — the `-shm` equivalent of `ShmMap::open`'s old length check,
/// still needed because a crash-truncated or half-written `-shm` file must
/// be rejected with a structured `Err`, not read out of bounds.
fn validate_shm_len(file: &File) -> io::Result<()> {
    let len = file.metadata()?.len();
    if len < MIN_SHM_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "-shm file too short for a wal-index header",
        ));
    }
    Ok(())
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
        let _ = fcntl_lock(&self.file, libc::F_UNLCK, wal_read_lock_byte(self.slot), 1);
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
pub(crate) fn claim_wal_read_lock(shm_path: &Path) -> io::Result<Option<WalReadLock>> {
    let file = match OpenOptions::new().read(true).write(true).open(shm_path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    validate_shm_len(&file)?;

    let mut last_err = None;
    for slot in 1..=4usize {
        let byte = wal_read_lock_byte(slot);
        // Briefly exclusive, only long enough to publish this slot's mark
        // before downgrading to the SHARED lock held for the guard's
        // lifetime — matches SQLite's own claim sequence (spike 005 exp 4).
        match fcntl_lock(&file, libc::F_WRLCK, byte, 1) {
            Ok(()) => {
                set_read_mark(&file, slot, mx_frame(&file)?)?;
                fcntl_lock(&file, libc::F_RDLCK, byte, 1)?;
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

/// Test-only: whether `slot` on `shm_path` is currently free, probed via a
/// real second OS process (`src/vfs/test_lock_probe.rs`) — for tests
/// outside this module (e.g. `src/pager/mod.rs`) that need to observe
/// reader-mark lock state.
#[cfg(test)]
pub(crate) fn slot_is_free_test_only(shm_path: &Path, slot: usize) -> bool {
    super::test_lock_probe::lock_available(shm_path, "wrlock", wal_read_lock_byte(slot), 1)
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

    use crate::vfs::test_lock_probe::{hold_multiple, release_all};

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
        assert_eq!(read_mark(&file, guard.slot).unwrap(), 42);

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
    /// this.
    #[test]
    fn contended_slot_is_skipped_for_the_next_free_one() {
        let path = temp_shm(5);
        let held = hold_multiple(&path, &[("rdlock", wal_read_lock_byte(1), 1)]);

        let guard = claim_wal_read_lock(&path).unwrap().unwrap();
        assert_eq!(guard.slot, 2, "slot 1 is held, so slot 2 must be claimed");

        release_all(held);
        drop(guard);
        std::fs::remove_file(&path).unwrap();
    }

    /// When every reader slot is genuinely contended, `claim_wal_read_lock`
    /// must return `Err`, not silently succeed or panic — the failure mode
    /// a `Pager::open` caller needs to distinguish from success.
    #[test]
    fn all_slots_contended_returns_err() {
        let path = temp_shm(9);
        let held = hold_multiple(
            &path,
            &[1, 2, 3, 4].map(|slot| ("rdlock", wal_read_lock_byte(slot), 1)),
        );

        let result = claim_wal_read_lock(&path);
        assert!(result.is_err(), "expected Err, got {result:?}");

        release_all(held);
        std::fs::remove_file(&path).unwrap();
    }

    /// A `-shm` file shorter than the wal-index header must be rejected,
    /// not read out-of-bounds or panic — a realistic input for a
    /// crash-truncated or half-written `-shm` file. Since `-shm` access is
    /// now `pread`/`pwrite` rather than `mmap` (#66), this is also the
    /// scenario that used to risk `SIGBUS`: a truncated file now yields
    /// this structured `Err` instead.
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

        let held = hold_multiple(
            &shm_path,
            &[1, 2, 3, 4].map(|slot| ("rdlock", wal_read_lock_byte(slot), 1)),
        );

        let result = UnixVfs.claim_wal_read_lock(&db_path);
        match result {
            Err(VfsError::Locked { .. }) => {}
            other => panic!("expected VfsError::Locked, got {other:?}"),
        }

        release_all(held);
        std::fs::remove_file(&shm_path).unwrap();
    }
}
