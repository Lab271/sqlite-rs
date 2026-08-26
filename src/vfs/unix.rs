//! Unix `Vfs` implementation, backed by `std::fs`.

use std::cell::RefCell;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::sys::fcntl::{EACCES, EAGAIN};

use super::{companion_path, lock, shm, FileLock, Result, SharedLockGuard, Vfs, VfsError, VfsFile};

/// Reads database files directly from the local filesystem via `std::fs`.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnixVfs;

impl Vfs for UnixVfs {
    fn open_read(&self, path: &Path) -> Result<Box<dyn VfsFile>> {
        let file = File::open(path).map_err(|source| to_vfs_error(path, source))?;
        Ok(Box::new(UnixVfsFile::new(file, path)))
    }

    fn open_write(&self, path: &Path) -> Result<Box<dyn VfsFile>> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| to_vfs_error(path, source))?;
        Ok(Box::new(UnixVfsFile::new(file, path)))
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        path.try_exists()
            .map_err(|source| to_vfs_error(path, source))
    }

    fn create_or_open_write(&self, path: &Path) -> Result<Box<dyn VfsFile>> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| to_vfs_error(path, source))?;
        Ok(Box::new(UnixVfsFile::new(file, path)))
    }

    fn delete(&self, path: &Path) -> Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(to_vfs_error(path, source)),
        }
    }

    fn claim_wal_read_lock(&self, path: &Path) -> Result<Option<FileLock>> {
        let shm_path = companion_path(path, "-shm");
        shm::claim_wal_read_lock(&shm_path)
            .map(|opt| opt.map(|guard| FileLock(Box::new(guard))))
            .map_err(|source| to_lock_error(&shm_path, source))
    }

    fn claim_wal_checkpoint_lock(&self, path: &Path) -> Result<Option<FileLock>> {
        let shm_path = companion_path(path, "-shm");
        if !shm_path.exists() {
            return Ok(None);
        }
        shm::claim_wal_checkpoint_lock(&shm_path)
            .map(|guard| Some(FileLock(Box::new(guard))))
            .map_err(|source| to_lock_error(&shm_path, source))
    }

    fn active_wal_reader_marks(&self, path: &Path) -> Result<Vec<u32>> {
        let shm_path = companion_path(path, "-shm");
        if !shm_path.exists() {
            return Ok(Vec::new());
        }
        shm::active_reader_marks(&shm_path).map_err(|source| to_vfs_error(&shm_path, source))
    }

    fn publish_wal_backfill(&self, path: &Path, n_backfill: u32) -> Result<()> {
        let shm_path = companion_path(path, "-shm");
        if !shm_path.exists() {
            return Ok(());
        }
        shm::publish_backfill(&shm_path, n_backfill)
            .map_err(|source| to_vfs_error(&shm_path, source))
    }

    fn read_wal_backfill(&self, path: &Path) -> Result<u32> {
        let shm_path = companion_path(path, "-shm");
        if !shm_path.exists() {
            return Ok(0);
        }
        shm::read_backfill(&shm_path).map_err(|source| to_vfs_error(&shm_path, source))
    }

    fn claim_wal_write_lock(&self, path: &Path) -> Result<Option<FileLock>> {
        let shm_path = companion_path(path, "-shm");
        if !shm_path.exists() {
            return Ok(None);
        }
        shm::claim_wal_write_lock(&shm_path)
            .map(|guard| Some(FileLock(Box::new(guard))))
            .map_err(|source| to_lock_error(&shm_path, source))
    }

    fn publish_wal_mx_frame(&self, path: &Path, mx_frame: u32) -> Result<()> {
        let shm_path = companion_path(path, "-shm");
        if !shm_path.exists() {
            return Ok(());
        }
        shm::publish_mx_frame(&shm_path, mx_frame).map_err(|source| to_vfs_error(&shm_path, source))
    }

    fn open_wal_shm(&self, path: &Path) -> Result<Option<crate::vfs::AnyWalShm>> {
        let shm_path = companion_path(path, "-shm");
        if !shm_path.exists() {
            return Ok(None);
        }
        shm::open_wal_shm(&shm_path)
            .map(|handle| {
                Some(crate::vfs::AnyWalShm::from(
                    Box::new(handle) as Box<dyn crate::vfs::WalShm>
                ))
            })
            .map_err(|source| to_vfs_error(&shm_path, source))
    }
}

/// A single fd, shared (via `Rc`) between this file's I/O and any
/// [`FileLock`] `lock_shared` hands out — never a second, independently-
/// opened fd to the same path. `Pager::open`'s hot-journal recovery reads,
/// writes, and locks the main database file through this one handle end to
/// end, sidestepping the "`close()` drops all `fcntl` locks on the inode"
/// trap (POSIX `fcntl` locks are scoped to `(process, inode)`, not the open
/// file description — see [`lock::FileLockState::file`]).
struct UnixVfsFile {
    lock: Rc<RefCell<lock::FileLockState>>,
    path: PathBuf,
}

impl UnixVfsFile {
    fn new(file: File, path: &Path) -> Self {
        UnixVfsFile {
            lock: Rc::new(RefCell::new(lock::FileLockState::new(file))),
            path: path.to_path_buf(),
        }
    }
}

impl VfsFile for UnixVfsFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.lock
            .borrow()
            .file()
            .read_at(buf, offset)
            .map_err(|source| to_vfs_error(&self.path, source))
    }

    fn size(&self) -> Result<u64> {
        self.lock
            .borrow()
            .file()
            .metadata()
            .map(|m| m.len())
            .map_err(|source| to_vfs_error(&self.path, source))
    }

    fn lock_shared(&self) -> Result<FileLock> {
        self.lock
            .borrow_mut()
            .set_level(lock::LockLevel::Shared)
            .map_err(|source| to_lock_error(&self.path, source))?;
        Ok(FileLock(Box::new(UnixLockGuard {
            lock: Rc::clone(&self.lock),
            path: self.path.clone(),
        })))
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        self.lock
            .borrow()
            .file()
            .write_all_at(buf, offset)
            .map_err(|source| to_vfs_error(&self.path, source))
    }

    fn truncate(&self, len: u64) -> Result<()> {
        self.lock
            .borrow()
            .file()
            .set_len(len)
            .map_err(|source| to_vfs_error(&self.path, source))
    }

    fn sync(&self) -> Result<()> {
        self.lock
            .borrow()
            .file()
            .sync_data()
            .map_err(|source| to_vfs_error(&self.path, source))
    }
}

/// Returned by [`UnixVfsFile::lock_shared`]: holds the fd's shared lock
/// ladder at `Shared` (or, briefly, `Exclusive` for hot-journal recovery —
/// [`FileLock::escalate_to_exclusive`]) until dropped.
struct UnixLockGuard {
    lock: Rc<RefCell<lock::FileLockState>>,
    path: PathBuf,
}

impl SharedLockGuard for UnixLockGuard {
    fn check_reserved(&self) -> Result<bool> {
        self.lock
            .borrow()
            .check_reserved()
            .map_err(|source| to_vfs_error(&self.path, source))
    }

    fn escalate_to_exclusive(&mut self) -> Result<()> {
        self.lock
            .borrow_mut()
            .set_level(lock::LockLevel::Exclusive)
            .map_err(|source| to_lock_error(&self.path, source))
    }

    fn de_escalate_to_shared(&mut self) -> Result<()> {
        self.lock
            .borrow_mut()
            .set_level(lock::LockLevel::Shared)
            .map_err(|source| to_lock_error(&self.path, source))
    }

    fn set_level(&mut self, level: lock::LockLevel) -> Result<()> {
        self.lock
            .borrow_mut()
            .set_level(level)
            .map_err(|source| to_lock_error(&self.path, source))
    }
}

impl Drop for UnixLockGuard {
    fn drop(&mut self) {
        // Best-effort, matching `FileLockState`'s own `Drop`: a `drop`
        // can't propagate failure, and there is nothing more to do about
        // one anyway. The fd stays open via `UnixVfsFile`'s own `Rc`
        // clone — only the lock level this guard represents is released.
        self.lock
            .borrow_mut()
            .set_level(lock::LockLevel::Unlocked)
            .ok();
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
        Some(EAGAIN) | Some(EACCES) => VfsError::Locked {
            path: path.display().to_string(),
        },
        _ => to_vfs_error(path, source),
    }
}
