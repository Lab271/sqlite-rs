//! Virtual filesystem: the read path sqlite-rs uses to open and read
//! database files. Read-only for now (see issue #11) — locking and the
//! write path are deliberately out of scope here, but the trait is shaped
//! so a lock method can be added later without breaking it.
//!
//! This module is the designated `unsafe`/`dyn` boundary (see the
//! `mvl-limit` Makefile target): everything above the VFS stays in the
//! qualified subset.

mod memory;
mod page_source;
mod unix;

pub use memory::MemoryVfs;
pub use page_source::{PageError, PageSource, VfsPageSource};
pub use unix::UnixVfs;

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VfsError {
    #[error("file not found: {path}")]
    NotFound { path: String },

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

    /// Whether `path` exists — used to detect sibling `-wal` / `-journal`
    /// files.
    fn exists(&self, path: &Path) -> Result<bool>;
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
}

#[cfg(test)]
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
    }

    #[test]
    fn memory_vfs_contract() {
        let mut vfs = MemoryVfs::new();
        let contents = b"hello sqlite-rs vfs contract".to_vec();
        vfs.insert("/present.db", contents.clone());
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

        run_contract(UnixVfs, &present, &absent, &contents);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
