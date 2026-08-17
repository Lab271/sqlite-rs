//! In-memory `Vfs` implementation, for tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{FileLock, Result, SharedLockGuard, Vfs, VfsError, VfsFile};

/// An in-memory [`Vfs`] backed by a path -> bytes map. Lets tests exercise
/// `Vfs`-consuming code without touching the real filesystem. Contents are
/// `Arc<Mutex<..>>`-shared so a write through [`Vfs::open_write`] is visible
/// to any other handle on the same path, matching a real file's semantics
/// (#166 pager write path).
#[derive(Debug, Default, Clone)]
pub struct MemoryVfs {
    files: HashMap<PathBuf, Arc<Mutex<Vec<u8>>>>,
}

impl MemoryVfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a file's contents under `path`.
    pub fn insert(&mut self, path: impl Into<PathBuf>, contents: Vec<u8>) {
        self.files
            .insert(path.into(), Arc::new(Mutex::new(contents)));
    }

    fn handle(&self, path: &Path) -> Result<Arc<Mutex<Vec<u8>>>> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| VfsError::NotFound {
                path: path.display().to_string(),
            })
    }
}

impl Vfs for MemoryVfs {
    fn open_read(&self, path: &Path) -> Result<Box<dyn VfsFile>> {
        Ok(Box::new(MemoryVfsFile(self.handle(path)?)))
    }

    fn open_write(&self, path: &Path) -> Result<Box<dyn VfsFile>> {
        Ok(Box::new(MemoryVfsFile(self.handle(path)?)))
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        Ok(self.files.contains_key(path))
    }
}

struct MemoryVfsFile(Arc<Mutex<Vec<u8>>>);

/// The in-memory backend's `Mutex` is only ever contended within a single
/// test process and never crosses a panic boundary while held, so a
/// poisoned lock here indicates a bug in the test itself, not a condition
/// production code needs to recover from — surfaced as an ordinary I/O
/// error rather than a panic (`clippy::unwrap_used`/`panic` stay denied).
fn poisoned(path: &Path) -> VfsError {
    VfsError::Io {
        path: path.display().to_string(),
        source: std::io::Error::other("poisoned in-memory file lock"),
    }
}

impl VfsFile for MemoryVfsFile {
    #[allow(
        clippy::indexing_slicing,
        reason = "offset < data.len() is checked above; n = min(buf.len(), available.len()) is always in bounds on both sides"
    )]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        let data = self.0.lock().map_err(|_| poisoned(Path::new("<memory>")))?;
        let offset = offset as usize;
        if offset >= data.len() {
            return Ok(0);
        }
        let available = &data[offset..];
        let n = buf.len().min(available.len());
        buf[..n].copy_from_slice(&available[..n]);
        Ok(n)
    }

    fn size(&self) -> Result<u64> {
        let data = self.0.lock().map_err(|_| poisoned(Path::new("<memory>")))?;
        Ok(data.len() as u64)
    }

    fn lock_shared(&self) -> Result<FileLock> {
        Ok(FileLock(Box::new(NoopLock)))
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "offset..end is grown to end via resize just above, so it is always in bounds"
    )]
    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        let mut data = self.0.lock().map_err(|_| poisoned(Path::new("<memory>")))?;
        let offset = offset as usize;
        let end = offset.saturating_add(buf.len());
        if data.len() < end {
            data.resize(end, 0);
        }
        data[offset..end].copy_from_slice(buf);
        Ok(())
    }

    fn sync(&self) -> Result<()> {
        Ok(())
    }
}

/// The in-memory backend has no real file descriptor to lock — a no-op
/// satisfies the [`VfsFile`] contract for tests exercising `Vfs`-generic
/// code that also locks.
struct NoopLock;

impl SharedLockGuard for NoopLock {}
