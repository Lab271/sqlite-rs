//! In-memory `Vfs` implementation, for tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{FileLock, Result, SharedLockGuard, Vfs, VfsError, VfsFile};

/// An in-memory [`Vfs`] backed by a path -> bytes map. Lets tests exercise
/// `Vfs`-consuming code without touching the real filesystem.
#[derive(Debug, Default, Clone)]
pub struct MemoryVfs {
    files: HashMap<PathBuf, Arc<Vec<u8>>>,
}

impl MemoryVfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a file's contents under `path`.
    pub fn insert(&mut self, path: impl Into<PathBuf>, contents: Vec<u8>) {
        self.files.insert(path.into(), Arc::new(contents));
    }
}

impl Vfs for MemoryVfs {
    fn open_read(&self, path: &Path) -> Result<Box<dyn VfsFile>> {
        let contents = self
            .files
            .get(path)
            .cloned()
            .ok_or_else(|| VfsError::NotFound {
                path: path.display().to_string(),
            })?;
        Ok(Box::new(MemoryVfsFile(contents)))
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        Ok(self.files.contains_key(path))
    }
}

struct MemoryVfsFile(Arc<Vec<u8>>);

impl VfsFile for MemoryVfsFile {
    #[allow(
        clippy::indexing_slicing,
        reason = "offset < self.0.len() is checked above; n = min(buf.len(), available.len()) is always in bounds on both sides"
    )]
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        let offset = offset as usize;
        if offset >= self.0.len() {
            return Ok(0);
        }
        let available = &self.0[offset..];
        let n = buf.len().min(available.len());
        buf[..n].copy_from_slice(&available[..n]);
        Ok(n)
    }

    fn size(&self) -> Result<u64> {
        Ok(self.0.len() as u64)
    }

    fn lock_shared(&self) -> Result<FileLock> {
        Ok(FileLock(Box::new(NoopLock)))
    }
}

/// The in-memory backend has no real file descriptor to lock — a no-op
/// satisfies the [`VfsFile`] contract for tests exercising `Vfs`-generic
/// code that also locks.
struct NoopLock;

impl SharedLockGuard for NoopLock {}
