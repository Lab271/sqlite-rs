// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Raw POSIX fcntl byte-range locking, mirroring SQLite's os_unix.c exactly.
//!
//! Byte offsets verified against the real SQLite source (github.com/sqlite/sqlite,
//! src/os_unix.c and src/wal.c), not recollection — see findings.md.

use libc::{c_int, flock, off_t};
use std::io;
use std::os::unix::io::RawFd;

// --- Journal-mode lock byte page (main db file) ---
pub const PENDING_BYTE: off_t = 0x40000000;
#[allow(dead_code)] // documents the full lock-byte layout; unused by these experiments
pub const RESERVED_BYTE: off_t = PENDING_BYTE + 1;
pub const SHARED_FIRST: off_t = PENDING_BYTE + 2;
pub const SHARED_SIZE: off_t = 510;

// --- WAL shared-memory (-shm) lock bytes ---
pub const UNIX_SHM_BASE: off_t = 120;
#[allow(dead_code)] // documents the full lock-byte layout; unused by these experiments
pub const WAL_WRITE_LOCK: off_t = UNIX_SHM_BASE;
#[allow(dead_code)]
pub const WAL_CKPT_LOCK: off_t = UNIX_SHM_BASE + 1;
#[allow(dead_code)]
pub const WAL_RECOVER_LOCK: off_t = UNIX_SHM_BASE + 2;
#[allow(dead_code)]
pub const UNIX_SHM_DMS: off_t = UNIX_SHM_BASE + 8;

pub fn wal_read_lock_byte(slot: off_t) -> off_t {
    UNIX_SHM_BASE + 3 + slot
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockAttempt {
    Acquired,
    Blocked,
}

/// Non-blocking fcntl byte-range lock attempt (F_SETLK — never waits).
/// This is exactly the primitive SQLite itself uses; a `Blocked` result
/// here is an `EAGAIN`/`EACCES` from the kernel, not a timeout guess.
pub fn try_lock(fd: RawFd, kind: c_int, start: off_t, len: off_t) -> io::Result<LockAttempt> {
    let mut fl: flock = unsafe { std::mem::zeroed() };
    fl.l_type = kind as _;
    fl.l_whence = libc::SEEK_SET as _;
    fl.l_start = start;
    fl.l_len = len;
    let ret = unsafe { libc::fcntl(fd, libc::F_SETLK, &fl) };
    if ret == 0 {
        Ok(LockAttempt::Acquired)
    } else {
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EAGAIN) | Some(libc::EACCES) => Ok(LockAttempt::Blocked),
            _ => Err(err),
        }
    }
}

pub fn unlock(fd: RawFd, start: off_t, len: off_t) -> io::Result<()> {
    let mut fl: flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_UNLCK as _;
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
