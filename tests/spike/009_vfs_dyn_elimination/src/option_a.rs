// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Option A prototype: associated-type `Vfs`, no `dyn`.
//!
//! Slice under test: `Vfs`/`VfsFile`/`PageSource`/`VfsPageSource`, mirroring
//! src/vfs.rs, src/vfs/page_source.rs, src/vfs/{unix,memory}.rs, and the one
//! real consumer (src/pager.rs's `Pager`). `SharedLockGuard`/`FileLock` are
//! out of scope per the issue.

use std::path::Path;

// ---- trait definitions (was src/vfs.rs) ----

pub trait VfsFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize, String>;
    fn size(&self) -> Result<u64, String>;
}

pub trait Vfs {
    type File: VfsFile;
    fn open_read(&self, path: &Path) -> Result<Self::File, String>;
    fn exists(&self, path: &Path) -> Result<bool, String>;
}

// ---- PageSource (was src/vfs/page_source.rs) ----
// Churn point #1: PageSource/VfsPageSource pick up a type parameter.

pub trait PageSource {
    fn read_page(&self, page_num: u32) -> Result<Vec<u8>, String>;
}

pub struct VfsPageSource<F: VfsFile> {
    file: F,
    page_size: u32,
}

impl<F: VfsFile> VfsPageSource<F> {
    pub fn open<V: Vfs<File = F>>(vfs: &V, path: &Path, page_size: u32) -> Result<Self, String> {
        let file = vfs.open_read(path)?;
        Ok(VfsPageSource { file, page_size })
    }
}

impl<F: VfsFile> PageSource for VfsPageSource<F> {
    fn read_page(&self, page_num: u32) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; self.page_size as usize];
        let offset = (page_num as u64 - 1) * self.page_size as u64;
        self.file.read_at(&mut buf, offset)?;
        Ok(buf)
    }
}

// ---- Two concrete backends (was src/vfs/{unix,memory}.rs) ----

pub struct UnixFile {
    path: std::path::PathBuf,
}
impl VfsFile for UnixFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize, String> {
        let _ = (&self.path, offset);
        Ok(buf.len())
    }
    fn size(&self) -> Result<u64, String> {
        Ok(4096)
    }
}

pub struct UnixVfs;
impl Vfs for UnixVfs {
    type File = UnixFile;
    fn open_read(&self, path: &Path) -> Result<UnixFile, String> {
        Ok(UnixFile {
            path: path.to_path_buf(),
        })
    }
    fn exists(&self, _path: &Path) -> Result<bool, String> {
        Ok(true)
    }
}

pub struct MemoryFile {
    contents: Vec<u8>,
}
impl VfsFile for MemoryFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize, String> {
        let n = buf.len().min(self.contents.len().saturating_sub(offset as usize));
        buf[..n].copy_from_slice(&self.contents[offset as usize..offset as usize + n]);
        Ok(n)
    }
    fn size(&self) -> Result<u64, String> {
        Ok(self.contents.len() as u64)
    }
}

pub struct MemoryVfs;
impl Vfs for MemoryVfs {
    type File = MemoryFile;
    fn open_read(&self, _path: &Path) -> Result<MemoryFile, String> {
        Ok(MemoryFile {
            contents: vec![0u8; 16],
        })
    }
    fn exists(&self, _path: &Path) -> Result<bool, String> {
        Ok(true)
    }
}

// ---- Consumer (was src/pager.rs's Pager) ----
// Churn point #2: Pager<V: Vfs> now generic over the backend.

pub struct Pager<V: Vfs> {
    source: VfsPageSource<V::File>,
}

impl<V: Vfs> Pager<V> {
    pub fn open(vfs: &V, path: &Path, page_size: u32) -> Result<Self, String> {
        Ok(Pager {
            source: VfsPageSource::open(vfs, path, page_size)?,
        })
    }

    pub fn read_page(&self, page_num: u32) -> Result<Vec<u8>, String> {
        self.source.read_page(page_num)
    }
}

// ---- Runtime backend selection (con: needs its own erasure layer) ----
// A CLI flag picking Unix vs Memory at runtime can't return `Pager<V>` for
// a `V` chosen dynamically without an enum or `dyn` *somewhere* above this
// layer. Demonstrated here as an enum, which is exactly Option B's shape —
// this is the "erasure moves, doesn't disappear" con from the issue.
pub enum AnyPager {
    Unix(Pager<UnixVfs>),
    Memory(Pager<MemoryVfs>),
}

impl AnyPager {
    pub fn read_page(&self, page_num: u32) -> Result<Vec<u8>, String> {
        match self {
            AnyPager::Unix(p) => p.read_page(page_num),
            AnyPager::Memory(p) => p.read_page(page_num),
        }
    }
}

fn main() {
    let unix_pager: Pager<UnixVfs> = Pager::open(&UnixVfs, Path::new("/tmp/x.db"), 4096).unwrap();
    println!("{}", unix_pager.read_page(1).unwrap().len());

    let mem_pager: Pager<MemoryVfs> =
        Pager::open(&MemoryVfs, Path::new("/mem.db"), 16).unwrap();
    println!("{}", mem_pager.read_page(1).unwrap().len());

    let chosen: AnyPager = AnyPager::Memory(mem_pager);
    println!("{}", chosen.read_page(1).unwrap().len());
}
