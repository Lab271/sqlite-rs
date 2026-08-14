//! Journal-mode SHARED byte-range locking via raw POSIX `fcntl`. This is
//! the only place in the crate where `unsafe` is needed for locking — see
//! `src/lib.rs`'s `#![deny(unsafe_code)]` and the Makefile's `mvl-limit`
//! boundary-policy comment, which already designates all of `src/vfs/` as
//! the qualified-subset gate's `unsafe`/`dyn` boundary.
//!
//! Byte offsets verified against SQLite's own source (`os_unix.c`) by
//! spike 005 (`tests/spike/005_locking_interop/findings.md`) — not
//! re-derived here. Busy detection is mapped one layer up, in
//! `src/vfs/unix.rs`'s `to_lock_error`. The WAL `-shm` reader-mark protocol
//! is out of scope for this module; see #45.
#![allow(unsafe_code)]

use std::io;
use std::os::unix::io::RawFd;

use libc::{c_int, flock, off_t};

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
/// released on drop.
pub struct UnixSharedLock {
    fd: RawFd,
}

impl SharedLockGuard for UnixSharedLock {}

impl Drop for UnixSharedLock {
    fn drop(&mut self) {
        // Best-effort: `drop` can't propagate a failure, and there is
        // nothing more this crate can do about one anyway.
        let _ = fcntl_lock(self.fd, libc::F_UNLCK as c_int, SHARED_FIRST, SHARED_SIZE);
    }
}

/// Acquires a non-blocking SHARED lock on `fd`'s journal-mode lock-byte
/// range. `Err` on any failure, including lock contention (`EAGAIN`/
/// `EACCES` surface here as a plain `io::Error`; `src/vfs/unix.rs`'s
/// `to_lock_error` is what turns those into a distinguishable "database is
/// locked" error one layer up).
pub fn lock_shared(fd: RawFd) -> io::Result<UnixSharedLock> {
    fcntl_lock(fd, libc::F_RDLCK as c_int, SHARED_FIRST, SHARED_SIZE)?;
    Ok(UnixSharedLock { fd })
}

/// Generic byte-range `fcntl(F_SETLK)` primitive — used both for the
/// journal-mode SHARED lock above and (via `pub(crate)`) for the WAL
/// `-shm` reader-mark lock bytes in `src/vfs/shm.rs`; the underlying
/// syscall is identical, only the byte offsets differ.
pub(crate) fn fcntl_lock(fd: RawFd, kind: c_int, start: off_t, len: off_t) -> io::Result<()> {
    let mut fl: flock = unsafe { std::mem::zeroed() };
    fl.l_type = kind as _;
    fl.l_whence = libc::SEEK_SET as _;
    fl.l_start = start;
    fl.l_len = len;
    let ret = unsafe { libc::fcntl(fd, libc::F_SETLK, &fl) };
    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Test-only: forks a child process that attempts a non-blocking
/// EXCLUSIVE lock on `path`'s SHARED-lock byte range and reports whether
/// it succeeded. A real second process is required because POSIX record
/// locks are scoped to (process, inode) — a lock already held by this
/// process never conflicts with a further request from this same
/// process, so in-process re-locking can't observe it.
#[cfg(test)]
#[allow(
    clippy::panic,
    reason = "test-only helper: a fork failure has no reasonable fallback"
)]
pub(crate) fn exclusive_lock_available(path: &std::path::Path) -> bool {
    use std::os::unix::io::AsRawFd;

    // Safety: the child only performs async-signal-safe work (a raw
    // `fcntl` syscall, no allocation) before `_exit`, avoiding the
    // classic post-`fork` hazard of a multithreaded parent's runtime
    // state (e.g. an allocator lock) being left inconsistent in the
    // child.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => panic!("fork failed: {}", io::Error::last_os_error()),
        0 => {
            // `F_WRLCK` requires a writable fd (`fcntl(2)`) — a read-only
            // open fails with `EBADF` regardless of contention, which
            // would make this probe report "unavailable" unconditionally.
            let ok = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .ok()
                .map(|f| {
                    fcntl_lock(
                        f.as_raw_fd(),
                        libc::F_WRLCK as c_int,
                        SHARED_FIRST,
                        SHARED_SIZE,
                    )
                    .is_ok()
                })
                .unwrap_or(false);
            unsafe { libc::_exit(if ok { 0 } else { 1 }) };
        }
        pid => {
            let mut status: c_int = 0;
            unsafe { libc::waitpid(pid, &mut status, 0) };
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
        }
    }
}

/// Test-only: forks a child process that takes a blocking EXCLUSIVE lock
/// on `path`'s SHARED-lock byte range, signals once held, runs `during`
/// in the parent, then releases the child and waits for it to exit. Lets
/// a test observe how `lock_shared` behaves against a lock genuinely held
/// by a separate OS process (in-process re-locking never conflicts, since
/// POSIX record locks are scoped to (process, inode)).
#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test-only helper: a fork failure has no reasonable fallback"
)]
pub(crate) fn with_exclusive_lock_held_by_child<R>(
    path: &std::path::Path,
    during: impl FnOnce() -> R,
) -> R {
    use std::os::unix::io::AsRawFd;

    let (parent_sock, child_sock) = std::os::unix::net::UnixDatagram::pair().unwrap();

    // Safety: the child only performs async-signal-safe work (raw fcntl /
    // socket syscalls, no allocation) before `_exit`, avoiding the classic
    // post-`fork` hazard of a multithreaded parent's runtime state (e.g.
    // an allocator lock) being left inconsistent in the child.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => panic!("fork failed: {}", io::Error::last_os_error()),
        0 => {
            drop(parent_sock);
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .expect("child opens file for locking");
            fcntl_lock(
                file.as_raw_fd(),
                libc::F_WRLCK as c_int,
                SHARED_FIRST,
                SHARED_SIZE,
            )
            .expect("child takes exclusive lock");
            child_sock.send(b"locked").expect("child signals locked");
            let mut buf = [0u8; 4];
            let _ = child_sock.recv(&mut buf);
            unsafe { libc::_exit(0) };
        }
        pid => {
            drop(child_sock);
            let mut buf = [0u8; 16];
            parent_sock.recv(&mut buf).expect("wait for child lock");

            let result = during();

            parent_sock.send(b"done").expect("release child");
            let mut status: c_int = 0;
            unsafe { libc::waitpid(pid, &mut status, 0) };
            result
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

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
        assert!(lock_shared(file.as_raw_fd()).is_ok());
    }

    #[test]
    fn shared_lock_blocks_concurrent_exclusive_lock_until_dropped() {
        let (file, path) = temp_file();
        let guard = lock_shared(file.as_raw_fd()).unwrap();

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

        let result = with_exclusive_lock_held_by_child(&path, || {
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
