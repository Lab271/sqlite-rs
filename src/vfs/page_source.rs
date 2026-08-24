//! Whole-page reads on top of [`VfsFile::read_at`], for the b-tree layer.
//!
//! [`PageSource`] is defined here (not in `src/btree/`) so the b-tree
//! module can depend on it generically without ever writing `dyn` itself —
//! `src/btree/` is not exempt from the `mvl-limit` gate; only this module
//! is (see the Makefile's qualified-subset gate comment).

use std::path::Path;
use std::rc::Rc;

use thiserror::Error;

use super::{AnyVfsFile, FileLock, Vfs, VfsError, VfsFile};

/// Failure reading or writing a whole page through a [`PageSource`].
#[derive(Debug, Error)]
pub enum PageError {
    /// Page numbers are 1-based; page 0 was requested.
    #[error("invalid page number 0")]
    InvalidPageNumber,

    /// A read returned fewer bytes than a full page.
    #[error("short read on page {page_num}: expected {expected} bytes, got {got}")]
    ShortRead {
        /// The page that came up short.
        page_num: u32,
        /// The page size that was expected.
        expected: usize,
        /// The number of bytes actually read.
        got: usize,
    },

    /// [`WritablePageSource::write_page`] was given a buffer that isn't
    /// exactly one page long.
    #[error("wrong buffer length writing page {page_num}: expected {expected} bytes, got {got}")]
    WrongLength {
        /// The page that was being written.
        page_num: u32,
        /// The page size that was expected.
        expected: usize,
        /// The length of the buffer actually given.
        got: usize,
    },

    /// The underlying VFS operation failed.
    #[error(transparent)]
    Vfs(#[from] VfsError),
}

/// Reads page `page_num` from `file`, shared between [`VfsPageSource`] and
/// [`WritablePageSource`].
fn read_page_at(file: &dyn VfsFile, page_size: u32, page_num: u32) -> Result<Rc<[u8]>, PageError> {
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
    Ok(Rc::from(buf))
}

/// A source of whole database pages, numbered from 1. Returns [`Rc<[u8]>`]
/// rather than `Vec<u8>` so a cache hit (the common case once a page is
/// warm — see `Pager`'s `page_cache`) is a refcount bump, not a copy; the
/// b-tree read path (`src/btree.rs`'s `reassemble_payload`) leans on this
/// to avoid a per-row `Vec` allocation for the non-overflow case (#467).
pub trait PageSource {
    /// Reads page `page_num` (1-based) and returns exactly `page_size`
    /// bytes. `page_num == 0` or a short read is `Err`.
    fn read_page(&self, page_num: u32) -> Result<Rc<[u8]>, PageError>;
}

/// A [`PageSource`] backed by a [`VfsFile`] opened through a [`Vfs`].
pub struct VfsPageSource {
    file: Box<dyn VfsFile>,
    page_size: u32,
}

impl VfsPageSource {
    /// Opens `path` for reading through `vfs`.
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
    fn read_page(&self, page_num: u32) -> Result<Rc<[u8]>, PageError> {
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
    /// Opens `path` for reading and writing through `vfs`.
    pub fn open(vfs: &dyn Vfs, path: &Path, page_size: u32) -> Result<Self, VfsError> {
        let file = vfs.open_write(path)?;
        Ok(WritablePageSource { file, page_size })
    }

    /// Wraps an already-opened file handle rather than opening a fresh one
    /// — for `Pager::open`'s hot-journal recovery (#359), which must probe
    /// and escalate the lock on, then read/write/truncate, the *same* fd
    /// used for every page access afterward. A second independently-opened
    /// fd to the same path would reintroduce the "`close()` drops all
    /// `fcntl` locks on the inode" trap this struct's own doc comment
    /// above already commits to avoiding.
    pub fn from_file(file: AnyVfsFile, page_size: u32) -> Self {
        WritablePageSource {
            file: file.into_inner(),
            page_size,
        }
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
    fn read_page(&self, page_num: u32) -> Result<Rc<[u8]>, PageError> {
        read_page_at(self.file.as_ref(), self.page_size, page_num)
    }
}

/// Lets the VDBE (`src/vdbe/cursor.rs`) share one page source across
/// several `TableCursor`s (one per open `OpenRead` cursor slot) without
/// cloning the underlying file handle — `Rc` is cheap to clone, and the
/// VM is single-threaded, so this never needs to be `Send`/`Sync`.
impl PageSource for std::rc::Rc<dyn PageSource> {
    fn read_page(&self, page_num: u32) -> Result<Rc<[u8]>, PageError> {
        (**self).read_page(page_num)
    }
}

/// Lets a `TableCursor` borrow a page source by shared reference instead
/// of consuming it — needed by schema write helpers (`src/btree/master.rs`,
/// #193) that scan a table (e.g. `sqlite_master`) through `&Pager` while
/// still holding the same `Pager` for a later mutable write.
// If `PageSource` grows a second method, forward it here too — the
// compiler won't warn about a missing forward on a trait with only one
// method.
impl<T: PageSource + ?Sized> PageSource for &T {
    fn read_page(&self, page_num: u32) -> Result<Rc<[u8]>, PageError> {
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
