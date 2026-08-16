//! Option B prototype: closed enum dispatch, no `dyn`, no generics.
//!
//! Same slice as option_a.rs: `Vfs`/`VfsFile`/`PageSource`/`VfsPageSource`
//! plus the one real consumer (`Pager`).

use std::path::Path;

// ---- concrete file backends (was src/vfs/{unix,memory}.rs) ----

struct RawUnixFile {
    path: std::path::PathBuf,
}
impl RawUnixFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize, String> {
        let _ = (&self.path, offset);
        Ok(buf.len())
    }
    fn size(&self) -> Result<u64, String> {
        Ok(4096)
    }
}

struct RawMemoryFile {
    contents: Vec<u8>,
}
impl RawMemoryFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize, String> {
        let n = buf.len().min(self.contents.len().saturating_sub(offset as usize));
        buf[..n].copy_from_slice(&self.contents[offset as usize..offset as usize + n]);
        Ok(n)
    }
    fn size(&self) -> Result<u64, String> {
        Ok(self.contents.len() as u64)
    }
}

// ---- closed-enum VfsFile (was `trait VfsFile` + `Box<dyn VfsFile>`) ----
// Churn point #1: every VfsFile method needs a match arm here.

pub enum AnyVfsFile {
    Unix(RawUnixFile),
    Memory(RawMemoryFile),
}

impl AnyVfsFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize, String> {
        match self {
            AnyVfsFile::Unix(f) => f.read_at(buf, offset),
            AnyVfsFile::Memory(f) => f.read_at(buf, offset),
        }
    }
    #[allow(dead_code)]
    fn size(&self) -> Result<u64, String> {
        match self {
            AnyVfsFile::Unix(f) => f.size(),
            AnyVfsFile::Memory(f) => f.size(),
        }
    }
}

// ---- closed-enum Vfs (was `trait Vfs` + `dyn Vfs`) ----
// Churn point #2: every Vfs method needs a match arm here too.

pub enum AnyVfs {
    Unix,
    Memory,
}

impl AnyVfs {
    fn open_read(&self, path: &Path) -> Result<AnyVfsFile, String> {
        match self {
            AnyVfs::Unix => Ok(AnyVfsFile::Unix(RawUnixFile {
                path: path.to_path_buf(),
            })),
            AnyVfs::Memory => Ok(AnyVfsFile::Memory(RawMemoryFile {
                contents: vec![0u8; 16],
            })),
        }
    }
    #[allow(dead_code)]
    fn exists(&self, _path: &Path) -> Result<bool, String> {
        Ok(true)
    }
}

// ---- PageSource (was src/vfs/page_source.rs) ----
// No churn: VfsPageSource stays concrete, holds the enum directly.

pub struct VfsPageSource {
    file: AnyVfsFile,
    page_size: u32,
}

impl VfsPageSource {
    pub fn open(vfs: &AnyVfs, path: &Path, page_size: u32) -> Result<Self, String> {
        let file = vfs.open_read(path)?;
        Ok(VfsPageSource { file, page_size })
    }

    pub fn read_page(&self, page_num: u32) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; self.page_size as usize];
        let offset = (page_num as u64 - 1) * self.page_size as u64;
        self.file.read_at(&mut buf, offset)?;
        Ok(buf)
    }
}

// ---- Consumer (was src/pager.rs's Pager) ----
// No churn: Pager stays non-generic, exactly as it is on main today.

pub struct Pager {
    source: VfsPageSource,
}

impl Pager {
    pub fn open(vfs: &AnyVfs, path: &Path, page_size: u32) -> Result<Self, String> {
        Ok(Pager {
            source: VfsPageSource::open(vfs, path, page_size)?,
        })
    }

    pub fn read_page(&self, page_num: u32) -> Result<Vec<u8>, String> {
        self.source.read_page(page_num)
    }
}

fn main() {
    let unix_pager = Pager::open(&AnyVfs::Unix, Path::new("/tmp/x.db"), 4096).unwrap();
    println!("{}", unix_pager.read_page(1).unwrap().len());

    let mem_pager = Pager::open(&AnyVfs::Memory, Path::new("/mem.db"), 16).unwrap();
    println!("{}", mem_pager.read_page(1).unwrap().len());

    // Runtime backend selection is free here — no separate erasure layer
    // needed, `AnyVfs`/`AnyVfsFile` already is the erasure layer.
    let chosen: AnyVfs = AnyVfs::Memory;
    let p = Pager::open(&chosen, Path::new("/mem.db"), 16).unwrap();
    println!("{}", p.read_page(1).unwrap().len());
}
