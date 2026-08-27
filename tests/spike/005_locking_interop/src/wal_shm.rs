// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Minimal reader/writer for the `-shm` wal-index header, per the exact
//! layout of `WalIndexHdr` + `WalCkptInfo` in SQLite's wal.c:
//!
//! ```text
//! offset  field
//! 0       WalIndexHdr copy 1 (48 bytes): iVersion,unused,iChange u32;
//!         isInit,bigEndCksum u8; szPage u16; mxFrame,nPage u32;
//!         aFrameCksum[2],aSalt[2],aCksum[2] u32
//! 48      WalIndexHdr copy 2 (48 bytes, identical layout)
//! 96      WalCkptInfo.nBackfill (u32)
//! 100     WalCkptInfo.aReadMark[5] (u32 x5)
//! 120     WalCkptInfo.aLock[8]        <- matches UNIX_SHM_BASE == 120
//! 128     WalCkptInfo.nBackfillAttempted (u32)
//! ```

use libc::{c_void, off_t, MAP_SHARED, PROT_READ, PROT_WRITE};
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::ptr;

pub const MX_FRAME_OFFSET: isize = 16;
pub const READ_MARK_BASE_OFFSET: isize = 100;
pub const READ_MARK_UNUSED: u32 = 0xFFFF_FFFF;

pub struct ShmMap {
    ptr: *mut u8,
    len: usize,
    _file: File,
}

impl ShmMap {
    pub fn open(shm_path: &str) -> Self {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(shm_path)
            .expect("open -shm file failed");
        let len = file.metadata().expect("stat -shm failed").len() as usize;
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
        assert_ne!(ptr, libc::MAP_FAILED, "mmap of -shm failed");
        ShmMap {
            ptr: ptr as *mut u8,
            len,
            _file: file,
        }
    }

    unsafe fn read_u32(&self, offset: isize) -> u32 {
        let p = self.ptr.offset(offset) as *const u32;
        ptr::read_unaligned(p)
    }

    unsafe fn write_u32(&self, offset: isize, value: u32) {
        let p = self.ptr.offset(offset) as *mut u32;
        ptr::write_unaligned(p, value);
    }

    /// mxFrame from wal-index header copy 1 — valid once `isInit` is set,
    /// which is always true right after sqlite3 has written anything in WAL mode.
    pub fn mx_frame(&self) -> u32 {
        unsafe { self.read_u32(MX_FRAME_OFFSET) }
    }

    pub fn read_mark(&self, slot: usize) -> u32 {
        unsafe { self.read_u32(READ_MARK_BASE_OFFSET + (slot as isize) * 4) }
    }

    pub fn set_read_mark(&self, slot: usize, value: u32) {
        unsafe { self.write_u32(READ_MARK_BASE_OFFSET + (slot as isize) * 4, value) }
    }
}

impl Drop for ShmMap {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut c_void, self.len);
        }
    }
}
