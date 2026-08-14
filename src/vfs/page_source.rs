//! Whole-page reads on top of [`VfsFile::read_at`], for the b-tree layer.
//!
//! [`PageSource`] is defined here (not in `src/btree/`) so the b-tree
//! module can depend on it generically without ever writing `dyn` itself —
//! `src/btree/` is not exempt from the `mvl-limit` gate; only this module
//! is (see the Makefile's qualified-subset gate comment).

use std::path::Path;

use thiserror::Error;

use super::{Vfs, VfsError, VfsFile};

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
}

impl PageSource for VfsPageSource {
    fn read_page(&self, page_num: u32) -> Result<Vec<u8>, PageError> {
        if page_num == 0 {
            return Err(PageError::InvalidPageNumber);
        }
        let mut buf = vec![0u8; self.page_size as usize];
        let offset = (page_num as u64 - 1) * self.page_size as u64;
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
