//! Journal-mode SHARED byte-range locking via `nix::fcntl` (safe wrapper
//! over POSIX `fcntl(F_SETLK)`). `src/vfs/` used to be the crate's sole
//! `#![allow(unsafe_code)]` carve-out (see the Makefile's `mvl-limit`
//! boundary-policy comment); with this module's `unsafe fcntl`/`mmap`/
//! `fork` calls replaced by safe `nix`/`std` APIs, `src/lib.rs` now
//! `#![forbid(unsafe_code)]` crate-wide (#66).
//!
//! Byte offsets verified against SQLite's own source (`os_unix.c`) by
//! spike 005 (`tests/spike/005_locking_interop/findings.md`) — not
//! re-derived here. Busy detection is mapped one layer up, in
//! `src/vfs/unix.rs`'s `to_lock_error`. The WAL `-shm` reader-mark protocol
//! is out of scope for this module; see #45.

use std::fs::File;
use std::io;

use nix::fcntl::{fcntl, FcntlArg};
use nix::libc::{self, off_t};

use super::SharedLockGuard;

/// SQLite's `PENDING_BYTE` (`os_unix.c`): base of the reserved lock-byte
/// page.
const PENDING_BYTE: off_t = 0x40000000;
/// `SHARED_FIRST` (`os_unix.c`): first byte of the SHARED-lock range.
/// `PENDING_BYTE + 1` is `RESERVED_BYTE`, not used by a reader.
const SHARED_FIRST: off_t = PENDING_BYTE + 2;
/// `SHARED_SIZE` (`os_unix.c`): width of the SHARED-lock range.
const SHARED_SIZE: off_t = 510;

/// A held SHARED lock on a database file's journal-mode lock bytes,
/// released on drop. Holds its own duplicated `File` (via `try_clone`)
/// rather than a bare fd so releasing the lock never needs to reconstruct
/// an fd's validity out of thin air.
pub struct UnixSharedLock {
    file: File,
}

impl SharedLockGuard for UnixSharedLock {}

impl Drop for UnixSharedLock {
    fn drop(&mut self) {
        // Best-effort: `drop` can't propagate a failure, and there is
        // nothing more this crate can do about one anyway.
        fcntl_lock(&self.file, libc::F_UNLCK, SHARED_FIRST, SHARED_SIZE).ok();
    }
}

/// Acquires a non-blocking SHARED lock on `file`'s journal-mode lock-byte
/// range. `Err` on any failure, including lock contention (`EAGAIN`/
/// `EACCES` surface here as a plain `io::Error`; `src/vfs/unix.rs`'s
/// `to_lock_error` is what turns those into a distinguishable "database is
/// locked" error one layer up).
pub fn lock_shared(file: &File) -> io::Result<UnixSharedLock> {
    fcntl_lock(file, libc::F_RDLCK, SHARED_FIRST, SHARED_SIZE)?;
    Ok(UnixSharedLock {
        file: file.try_clone()?,
    })
}

/// Generic byte-range `fcntl(F_SETLK)` primitive — used both for the
/// journal-mode SHARED lock above and (via `pub(crate)`) for the WAL
/// `-shm` reader-mark lock bytes in `src/vfs/shm.rs`; the underlying
/// syscall is identical, only the byte offsets differ.
pub(crate) fn fcntl_lock(
    file: &File,
    kind: impl Into<i32>,
    start: off_t,
    len: off_t,
) -> io::Result<()> {
    // `libc::F_RDLCK`/`F_WRLCK`/`F_UNLCK` are `i16` on macOS but already
    // `i32` on Linux glibc — `Into<i32>` normalizes both without an `as`
    // cast or `i32::from` call visible at any call site, which clippy
    // would otherwise flag as redundant on whichever platform the
    // constant is already `i32`.
    let kind: i32 = kind.into();
    let fl = libc::flock {
        l_type: kind as _,
        l_whence: libc::SEEK_SET as _,
        l_start: start,
        l_len: len,
        l_pid: 0,
    };
    fcntl(file, FcntlArg::F_SETLK(&fl))
        .map(|_| ())
        .map_err(io::Error::from)
}

/// Test-only: whether a non-blocking EXCLUSIVE lock on `path`'s SHARED-lock
/// byte range would currently succeed, probed via a real second OS process
/// (`src/vfs/test_lock_probe.rs`) — needed by `src/pager/mod.rs`'s tests,
/// which observe `Pager::open`/`drop` lock state from outside this module.
#[cfg(test)]
pub(crate) fn exclusive_lock_available(path: &std::path::Path) -> bool {
    super::test_lock_probe::lock_available(path, "wrlock", SHARED_FIRST, SHARED_SIZE)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::vfs::test_lock_probe::lock_held_by_subprocess;

    fn temp_file() -> (std::fs::File, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("sqlite-rs-lock-test-{}-{n}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        (file, path)
    }

    #[test]
    fn lock_shared_succeeds_on_a_fresh_file() {
        let (file, _path) = temp_file();
        assert!(lock_shared(&file).is_ok());
    }

    #[test]
    fn shared_lock_blocks_concurrent_exclusive_lock_until_dropped() {
        let (file, path) = temp_file();
        let guard = lock_shared(&file).unwrap();

        assert!(
            !exclusive_lock_available(&path),
            "a held SHARED lock must block a concurrent EXCLUSIVE lock"
        );

        drop(guard);

        assert!(
            exclusive_lock_available(&path),
            "dropping the guard must release the SHARED lock"
        );

        std::fs::remove_file(&path).unwrap();
    }

    /// `lock_shared` contending with a real EXCLUSIVE lock held by another
    /// OS process (not just this process re-locking, which `fcntl` never
    /// sees as contention) must surface as lock contention (`EAGAIN`/
    /// `EACCES`), which `src/vfs/unix.rs`'s `to_lock_error` turns into
    /// `VfsError::Locked` — 001-architecture Req-4's busy-detection
    /// scenario.
    #[test]
    fn lock_shared_fails_with_contention_errno_when_exclusively_held_elsewhere() {
        use crate::vfs::{UnixVfs, Vfs, VfsError};

        let (_file, path) = temp_file();

        let result = lock_held_by_subprocess(&path, "wrlock", SHARED_FIRST, SHARED_SIZE, || {
            let file = UnixVfs.open_read(&path).unwrap();
            file.lock_shared()
        });

        match result {
            Err(VfsError::Locked { .. }) => {}
            Err(other) => panic!("expected VfsError::Locked, got {other:?}"),
            Ok(_) => panic!("expected VfsError::Locked, got Ok"),
        }

        std::fs::remove_file(&path).unwrap();
    }
}
