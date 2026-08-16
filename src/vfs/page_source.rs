//! Whole-page reads on top of [`VfsFile::read_at`], for the b-tree layer.
//!
//! [`PageSource`] is defined here (not in `src/btree/`) so the b-tree
//! module can depend on it generically without ever writing `dyn` itself —
//! `src/btree/` is not exempt from the `mvl-limit` gate; only this module
//! is (see the Makefile's qualified-subset gate comment).

use std::path::Path;

use thiserror::Error;

use super::{FileLock, Vfs, VfsError, VfsFile};

#[derive(Debug, Error)]
pub enum PageError {
    #[error("invalid page number 0")]
    InvalidPageNumber,

    #[error("short read on page {page_num}: expected {expected} bytes, got {got}")]
    ShortRead {
        page_num: u32,
        expected: usize,
        got: usize,
    },

    #[error(transparent)]
    Vfs(#[from] VfsError),
}

/// A source of whole database pages, numbered from 1.
pub trait PageSource {
    /// Reads page `page_num` (1-based) and returns exactly `page_size`
    /// bytes. `page_num == 0` or a short read is `Err`.
    fn read_page(&self, page_num: u32) -> Result<Vec<u8>, PageError>;
}

/// A [`PageSource`] backed by a [`VfsFile`] opened through a [`Vfs`].
pub struct VfsPageSource {
    file: Box<dyn VfsFile>,
    page_size: u32,
}

impl VfsPageSource {
    pub fn open(vfs: &dyn Vfs, path: &Path, page_size: u32) -> Result<Self, VfsError> {
        let file = vfs.open_read(path)?;
        Ok(VfsPageSource { file, page_size })
    }

    /// Acquires a SHARED lock on the underlying file — see
    /// [`VfsFile::lock_shared`].
    pub fn lock_shared(&self) -> Result<FileLock, VfsError> {
        self.file.lock_shared()
    }
}

impl PageSource for VfsPageSource {
    fn read_page(&self, page_num: u32) -> Result<Vec<u8>, PageError> {
        if page_num == 0 {
            return Err(PageError::InvalidPageNumber);
        }
        let mut buf = vec![0u8; self.page_size as usize];
        // page_num >= 1 here (checked above) and page_size is a validated
        // power of two in [512, 65536] (header.rs), so this product stays
        // far below u64::MAX; saturating_* just avoids asserting that by
        // inspection.
        let offset = (page_num as u64)
            .saturating_sub(1)
            .saturating_mul(self.page_size as u64);
        let n = self.file.read_at(&mut buf, offset)?;
        if n != buf.len() {
            return Err(PageError::ShortRead {
                page_num,
                expected: buf.len(),
                got: n,
            });
        }
        Ok(buf)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::vfs::MemoryVfs;
    use std::path::Path;

    #[test]
    fn page_zero_is_rejected() {
        let mut vfs = MemoryVfs::new();
        vfs.insert("/db", vec![0u8; 16]);
        let source = VfsPageSource::open(&vfs, Path::new("/db"), 16).unwrap();
        assert!(matches!(
            source.read_page(0),
            Err(PageError::InvalidPageNumber)
        ));
    }

    #[test]
    fn short_file_reports_short_read() {
        let mut vfs = MemoryVfs::new();
        vfs.insert("/db", vec![0u8; 8]);
        let source = VfsPageSource::open(&vfs, Path::new("/db"), 16).unwrap();
        match source.read_page(1) {
            Err(PageError::ShortRead {
                page_num,
                expected,
                got,
            }) => {
                assert_eq!(page_num, 1);
                assert_eq!(expected, 16);
                assert_eq!(got, 8);
            }
            other => panic!("expected ShortRead, got {other:?}"),
        }
    }
}
