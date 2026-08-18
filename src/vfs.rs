//! Virtual filesystem: the read path sqlite-rs uses to open and read
//! database files. Read-only for now (see issue #11) — the write path is
//! deliberately out of scope here. [`VfsFile::lock_shared`] (#50) acquires
//! the journal-mode SHARED lock a safe reader needs before serving pages,
//! surfacing lock contention as [`VfsError::Locked`] (#45). The WAL `-shm`
//! reader-mark protocol and the per-inode fd-cache for the
//! `close()`-drops-all-locks trap are further follow-up tracked in #45.
//!
//! This module is the designated `dyn` boundary (see the `mvl-limit`
//! Makefile target): everything above the VFS stays in the qualified
//! subset. It is no longer an `unsafe` boundary (#66) — `fcntl`/`-shm`
//! access here goes through safe `nix`/`std` APIs, and the crate is
//! `#![forbid(unsafe_code)]` with no local override anywhere.

pub(crate) mod lock;
mod memory;
mod page_source;
pub(crate) mod shm;
#[cfg(test)]
pub(crate) mod test_lock_probe;
mod unix;

pub use memory::MemoryVfs;
pub use page_source::{PageError, PageSource, VfsPageSource, WritablePageSource};
pub use unix::UnixVfs;

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VfsError {
    #[error("file not found: {path}")]
    NotFound { path: String },

    #[error("database is locked: {path}")]
    Locked { path: String },

    #[error("I/O error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, VfsError>;

/// A source of database files, opened by path.
pub trait Vfs {
    /// Opens `path` for reading.
    fn open_read(&self, path: &Path) -> Result<Box<dyn VfsFile>>;

    /// Opens `path` for reading and writing (#166 pager write path). The
    /// file must already exist — creating new database files is out of
    /// scope here. Callers that only ever read a page (e.g. b-tree
    /// scans through [`VfsPageSource`]) keep using [`Vfs::open_read`] so a
    /// genuinely read-only filesystem is never asked for write access it
    /// doesn't need.
    fn open_write(&self, path: &Path) -> Result<Box<dyn VfsFile>>;

    /// Whether `path` exists — used to detect sibling `-wal` / `-journal`
    /// files.
    fn exists(&self, path: &Path) -> Result<bool>;

    /// Opens `path` for reading and writing, creating it (empty) first if
    /// it doesn't already exist — used to create the `-journal` companion
    /// file on a transaction's first write (#172 rollback journal).
    fn create_or_open_write(&self, path: &Path) -> Result<Box<dyn VfsFile>>;

    /// Removes `path` if it exists; a no-op (not an error) if it doesn't —
    /// used to delete the `-journal` file on commit (#172, DELETE mode).
    fn delete(&self, path: &Path) -> Result<()>;

    /// Claims a WAL reader-mark slot on `path`'s `-shm` companion file (if
    /// one exists) so a live checkpointer backs off rather than
    /// backfilling/truncating WAL frames this reader depends on (#45).
    /// Released when the returned [`FileLock`] drops. Default: a no-op
    /// (`Ok(None)`) — correct for backends with no real `-shm` file to
    /// coordinate through, e.g. [`MemoryVfs`].
    fn claim_wal_read_lock(&self, path: &Path) -> Result<Option<FileLock>> {
        let _ = path;
        Ok(None)
    }
}

/// Builds the path of a companion file (e.g. `-wal`, `-journal`) by
/// appending `suffix` to `path`'s full name — never `.set_extension`, since
/// companion suffixes are appended after the existing `.db` extension, not
/// substituted for it (`test.db` + `-wal` = `test.db-wal`, not `test.wal`).
pub fn companion_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// A single file opened via [`Vfs::open_read`].
pub trait VfsFile {
    /// Reads into `buf` starting at `offset`, returning the number of bytes
    /// actually read (fewer than `buf.len()` at EOF).
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize>;

    /// The file's total size in bytes.
    fn size(&self) -> Result<u64>;

    /// Acquires a SHARED byte-range lock on the file's journal-mode lock
    /// bytes (`PENDING_BYTE+2` / `SHARED_SIZE`, matching SQLite's
    /// `os_unix.c`) so a concurrent writer can detect this reader per
    /// SQLite's rollback-journal lock ladder — validated to interop
    /// correctly with a live stock `sqlite3` process by spike 005
    /// (`tests/spike/005_locking_interop/findings.md`). Released when the
    /// returned guard is dropped.
    fn lock_shared(&self) -> Result<FileLock>;

    /// Writes `buf` at `offset`, extending the file if `offset + buf.len()`
    /// is past the current end (#166 pager write path).
    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()>;

    /// Truncates (or, if `len` is past the current end, extends with
    /// zeros) the file to exactly `len` bytes — used by rollback-journal
    /// recovery to shrink the main file back to its pre-transaction page
    /// count after replaying journaled pages (#172).
    fn truncate(&self, len: u64) -> Result<()>;

    /// Flushes any buffered writes to durable storage.
    fn sync(&self) -> Result<()>;
}

/// A boxed [`VfsFile`], for callers outside `src/vfs/` that need to hold
/// a file handle across several calls without naming `dyn` themselves —
/// same pattern as [`FileLock`] below, one trait earlier. The
/// rollback-journal write path (#172, `src/pager.rs`/`src/pager/journal.rs`)
/// is the motivating caller: it opens a `-journal`/main-file handle once
/// and writes to it across several method calls.
pub struct AnyVfsFile(Box<dyn VfsFile>);

impl From<Box<dyn VfsFile>> for AnyVfsFile {
    fn from(file: Box<dyn VfsFile>) -> Self {
        AnyVfsFile(file)
    }
}

impl AnyVfsFile {
    pub fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.0.read_at(buf, offset)
    }

    pub fn size(&self) -> Result<u64> {
        self.0.size()
    }

    pub fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        self.0.write_at(buf, offset)
    }

    pub fn truncate(&self, len: u64) -> Result<()> {
        self.0.truncate(len)
    }

    pub fn sync(&self) -> Result<()> {
        self.0.sync()
    }
}

/// A boxed [`Vfs`], for a long-lived struct outside `src/vfs/` that needs
/// to hold "the `Vfs` it was opened with" without itself naming `dyn` or
/// becoming generic over `V: Vfs` (`Pager`, #172 — it creates/deletes the
/// `-journal` companion file from methods called well after `open`
/// returns, once the original `&V` borrow is long gone).
pub struct AnyVfs(Box<dyn Vfs>);

impl AnyVfs {
    pub fn new<V: Vfs + 'static>(vfs: V) -> Self {
        AnyVfs(Box::new(vfs))
    }

    pub fn exists(&self, path: &Path) -> Result<bool> {
        self.0.exists(path)
    }

    pub fn open_read(&self, path: &Path) -> Result<AnyVfsFile> {
        self.0.open_read(path).map(AnyVfsFile::from)
    }

    pub fn open_write(&self, path: &Path) -> Result<AnyVfsFile> {
        self.0.open_write(path).map(AnyVfsFile::from)
    }

    pub fn create_or_open_write(&self, path: &Path) -> Result<AnyVfsFile> {
        self.0.create_or_open_write(path).map(AnyVfsFile::from)
    }

    pub fn delete(&self, path: &Path) -> Result<()> {
        self.0.delete(path)
    }
}

/// A held file lock, released when dropped. Opaque on purpose: it hides
/// `dyn SharedLockGuard` behind a concrete type so callers outside
/// `src/vfs/` (e.g. [`crate::pager::Pager`]) never need to write `dyn`
/// themselves — this module is the qualified-subset gate's designated
/// `dyn` boundary (see the `mvl-limit` Makefile target).
pub struct FileLock(
    #[allow(dead_code, reason = "held only for its Drop side effect")] Box<dyn SharedLockGuard>,
);

impl std::fmt::Debug for FileLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileLock(..)")
    }
}

/// Implemented next to each [`VfsFile`] backend (e.g. the Unix backend's
/// real `fcntl` lock, or a no-op for the in-memory backend).
trait SharedLockGuard {}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The contract every `Vfs` implementation must satisfy — run against
    /// both `UnixVfs` and `MemoryVfs` below.
    fn run_contract(vfs: impl Vfs, present: &Path, absent: &Path, contents: &[u8]) {
        assert!(vfs.exists(present).unwrap());
        assert!(!vfs.exists(absent).unwrap());
        assert!(vfs.open_read(absent).is_err());

        let file = vfs.open_read(present).unwrap();
        assert_eq!(file.size().unwrap(), contents.len() as u64);

        let mut buf = vec![0u8; contents.len()];
        let n = file.read_at(&mut buf, 0).unwrap();
        assert_eq!(n, contents.len());
        assert_eq!(buf, contents);

        let mut mid = vec![0u8; 4];
        let n = file.read_at(&mut mid, 2).unwrap();
        assert_eq!(n, 4);
        assert_eq!(mid, contents[2..6]);

        let mut past_eof = vec![0u8; 4];
        let n = file
            .read_at(&mut past_eof, contents.len() as u64 + 10)
            .unwrap();
        assert_eq!(n, 0);

        drop(file.lock_shared().unwrap());
    }

    /// A write through [`Vfs::open_write`] must be visible to a fresh
    /// [`Vfs::open_read`] handle on the same path — the contract every
    /// backend's write path must satisfy.
    fn run_write_contract(vfs: impl Vfs, path: &Path) {
        let write_file = vfs.open_write(path).unwrap();
        write_file.write_at(b"WXYZ", 2).unwrap();
        write_file.sync().unwrap();

        let read_file = vfs.open_read(path).unwrap();
        let mut buf = vec![0u8; 6];
        read_file.read_at(&mut buf, 0).unwrap();
        assert_eq!(&buf, b"heWXYZ");
    }

    #[test]
    fn memory_vfs_contract() {
        let mut vfs = MemoryVfs::new();
        let contents = b"hello sqlite-rs vfs contract".to_vec();
        vfs.insert("/present.db", contents.clone());
        vfs.insert("/writable.db", contents.clone());
        run_write_contract(vfs.clone(), Path::new("/writable.db"));
        run_contract(
            vfs,
            Path::new("/present.db"),
            Path::new("/absent.db"),
            &contents,
        );
    }

    #[test]
    fn companion_file_detection() {
        let mut vfs = MemoryVfs::new();
        vfs.insert("/test.db", b"main file".to_vec());
        vfs.insert("/test.db-wal", b"wal file".to_vec());
        vfs.insert("/test.db-journal", b"journal file".to_vec());

        assert!(vfs
            .exists(&companion_path(Path::new("/test.db"), "-wal"))
            .unwrap());
        assert!(vfs
            .exists(&companion_path(Path::new("/test.db"), "-journal"))
            .unwrap());
        assert!(!vfs
            .exists(&companion_path(Path::new("/other.db"), "-wal"))
            .unwrap());
    }

    #[test]
    fn unix_vfs_contract() {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("sqlite-rs-vfs-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let present = dir.join("present.db");
        let absent = dir.join("absent.db");
        let contents = b"hello sqlite-rs vfs contract".to_vec();
        std::fs::write(&present, &contents).unwrap();
        let writable = dir.join("writable.db");
        std::fs::write(&writable, &contents).unwrap();
        run_write_contract(UnixVfs, &writable);

        run_contract(UnixVfs, &present, &absent, &contents);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
