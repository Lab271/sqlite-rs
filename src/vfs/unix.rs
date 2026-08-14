//! Unix `Vfs` implementation, backed by `std::fs`.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use super::{companion_path, lock, shm, FileLock, Result, Vfs, VfsError, VfsFile};

/// Reads database files directly from the local filesystem via `std::fs`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnixVfs;

impl Vfs for UnixVfs {
    fn open_read(&self, path: &Path) -> Result<Box<dyn VfsFile>> {
        let file = File::open(path).map_err(|source| to_vfs_error(path, source))?;
        Ok(Box::new(UnixVfsFile {
            file,
            path: path.to_path_buf(),
        }))
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        path.try_exists()
            .map_err(|source| to_vfs_error(path, source))
    }

    fn claim_wal_read_lock(&self, path: &Path) -> Result<Option<FileLock>> {
        let shm_path = companion_path(path, "-shm");
        shm::claim_wal_read_lock(&shm_path)
            .map(|opt| opt.map(|guard| FileLock(Box::new(guard))))
            .map_err(|source| to_lock_error(&shm_path, source))
    }
}

struct UnixVfsFile {
    file: File,
    path: PathBuf,
}

impl VfsFile for UnixVfsFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.file
            .read_at(buf, offset)
            .map_err(|source| to_vfs_error(&self.path, source))
    }

    fn size(&self) -> Result<u64> {
        self.file
            .metadata()
            .map(|m| m.len())
            .map_err(|source| to_vfs_error(&self.path, source))
    }

    fn lock_shared(&self) -> Result<FileLock> {
        lock::lock_shared(self.file.as_raw_fd())
            .map(|guard| FileLock(Box::new(guard)))
            .map_err(|source| to_lock_error(&self.path, source))
    }
}

fn to_vfs_error(path: &Path, source: std::io::Error) -> VfsError {
    let path_str = path.display().to_string();
    if source.kind() == std::io::ErrorKind::NotFound {
        VfsError::NotFound { path: path_str }
    } else {
        VfsError::Io {
            path: path_str,
            source,
        }
    }
}

/// Like [`to_vfs_error`], but maps `fcntl(F_SETLK)`'s lock-contention errno
/// values (`EAGAIN`/`EACCES` — POSIX allows either, `fcntl(2)`) to
/// [`VfsError::Locked`] so callers can distinguish "another process holds
/// this lock" from an ordinary I/O failure.
fn to_lock_error(path: &Path, source: std::io::Error) -> VfsError {
    match source.raw_os_error() {
        Some(libc::EAGAIN) | Some(libc::EACCES) => VfsError::Locked {
            path: path.display().to_string(),
        },
        _ => to_vfs_error(path, source),
    }
}
