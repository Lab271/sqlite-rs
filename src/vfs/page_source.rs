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

    #[error("wrong buffer length writing page {page_num}: expected {expected} bytes, got {got}")]
    WrongLength {
        page_num: u32,
        expected: usize,
        got: usize,
    },

    #[error(transparent)]
    Vfs(#[from] VfsError),
}

/// Reads page `page_num` from `file`, shared between [`VfsPageSource`] and
/// [`WritablePageSource`].
fn read_page_at(file: &dyn VfsFile, page_size: u32, page_num: u32) -> Result<Vec<u8>, PageError> {
    if page_num == 0 {
        return Err(PageError::InvalidPageNumber);
    }
    let mut buf = vec![0u8; page_size as usize];
    // page_num >= 1 here (checked above) and page_size is a validated
    // power of two in [512, 65536] (header.rs), so this product stays
    // far below u64::MAX; saturating_* just avoids asserting that by
    // inspection.
    let offset = (page_num as u64)
        .saturating_sub(1)
        .saturating_mul(page_size as u64);
    let n = file.read_at(&mut buf, offset)?;
    if n != buf.len() {
        return Err(PageError::ShortRead {
            page_num,
            expected: buf.len(),
            got: n,
        });
    }
    Ok(buf)
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
        read_page_at(self.file.as_ref(), self.page_size, page_num)
    }
}

/// A [`PageSource`] backed by a read-write [`VfsFile`] opened through
/// [`Vfs::open_write`], adding [`WritablePageSource::write_page`] and
/// [`WritablePageSource::sync`] on top of the same single file handle used
/// for reads (#166 pager write path). Using one handle for both directions
/// — rather than a second fd opened alongside a read-only [`VfsPageSource`]
/// — sidesteps the documented "`close()` drops all `fcntl` locks on the
/// inode" trap (`src/pager.rs`'s module doc, #45): [`Pager`](crate::pager::Pager)
/// never opens a second fd to the same path, so there is nothing whose drop
/// could silently release a lock acquired through this one.
pub struct WritablePageSource {
    file: Box<dyn VfsFile>,
    page_size: u32,
}

impl WritablePageSource {
    pub fn open(vfs: &dyn Vfs, path: &Path, page_size: u32) -> Result<Self, VfsError> {
        let file = vfs.open_write(path)?;
        Ok(WritablePageSource { file, page_size })
    }

    /// Acquires a SHARED lock on the underlying file — see
    /// [`VfsFile::lock_shared`].
    pub fn lock_shared(&self) -> Result<FileLock, VfsError> {
        self.file.lock_shared()
    }

    /// Writes exactly `page_size` bytes of `bytes` as page `page_num`
    /// (1-based). `page_num == 0` or a wrong-length buffer is `Err`.
    pub fn write_page(&self, page_num: u32, bytes: &[u8]) -> Result<(), PageError> {
        if page_num == 0 {
            return Err(PageError::InvalidPageNumber);
        }
        if bytes.len() != self.page_size as usize {
            return Err(PageError::WrongLength {
                page_num,
                expected: self.page_size as usize,
                got: bytes.len(),
            });
        }
        let offset = (page_num as u64)
            .saturating_sub(1)
            .saturating_mul(self.page_size as u64);
        self.file.write_at(bytes, offset)?;
        Ok(())
    }

    /// Flushes all writes made via [`WritablePageSource::write_page`] to
    /// durable storage.
    pub fn sync(&self) -> Result<(), VfsError> {
        self.file.sync()
    }
}

impl PageSource for WritablePageSource {
    fn read_page(&self, page_num: u32) -> Result<Vec<u8>, PageError> {
        read_page_at(self.file.as_ref(), self.page_size, page_num)
    }
}

/// Lets the VDBE (`src/vdbe/cursor.rs`) share one page source across
/// several `TableCursor`s (one per open `OpenRead` cursor slot) without
/// cloning the underlying file handle — `Rc` is cheap to clone, and the
/// VM is single-threaded, so this never needs to be `Send`/`Sync`.
impl PageSource for std::rc::Rc<dyn PageSource> {
    fn read_page(&self, page_num: u32) -> Result<Vec<u8>, PageError> {
        (**self).read_page(page_num)
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
