// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
use super::freelist::FreelistError;
use super::journal::JournalError;
use super::wal::WalError;
use crate::vfs::{PageError, VfsError};

/// Errors surfaced by the pager while opening a database, replaying
/// recovery state, or servicing page reads/writes.
#[derive(Debug)]
pub enum PagerError {
    /// A hot rollback journal exists at `path`: the previous writer did not
    /// clean up, so the main database file may not reflect committed data.
    HotJournal {
        /// Path to the hot journal file that triggered the refusal.
        path: String,
    },

    /// The rollback journal itself failed to parse.
    Journal(JournalError),

    /// The write-ahead log at `path` failed to parse or validate.
    Wal {
        /// Path to the WAL file that failed.
        path: String,
        /// The underlying WAL parsing/validation error.
        source: WalError,
    },

    /// A page-level error propagated from the storage layer.
    Page(PageError),

    /// A VFS-level I/O or locking error.
    Vfs(VfsError),

    /// A freelist trunk/leaf page failed to parse.
    Freelist(FreelistError),

    /// `journal_mode` cannot be changed while a transaction is pending.
    PendingTransaction,

    /// Switching `journal_mode` out of WAL requires a checkpoint that fully
    /// back-fills the WAL into the main file first; the checkpoint left
    /// frames behind.
    CheckpointIncomplete,
}

impl std::fmt::Display for PagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PagerError::HotJournal { path } => write!(
                f,
                "hot rollback journal present at {path}: database was not cleanly closed and its main \
                 file may not reflect committed data; refusing to open read-only rather than risk \
                 serving pre-rollback pages as committed"
            ),
            PagerError::Journal(source) => write!(f, "rollback journal is corrupt: {source}"),
            PagerError::Wal { path, source } => write!(f, "reading WAL at {path}: {source}"),
            PagerError::Page(source) => write!(f, "{source}"),
            PagerError::Vfs(source) => write!(f, "{source}"),
            PagerError::Freelist(source) => write!(f, "{source}"),
            PagerError::PendingTransaction => {
                write!(f, "cannot change journal_mode with a pending transaction")
            }
            PagerError::CheckpointIncomplete => write!(
                f,
                "checkpoint did not fully back-fill the WAL while switching journal_mode out of WAL"
            ),
        }
    }
}

impl std::error::Error for PagerError {}

impl From<PageError> for PagerError {
    fn from(source: PageError) -> Self {
        PagerError::Page(source)
    }
}

impl From<VfsError> for PagerError {
    fn from(source: VfsError) -> Self {
        PagerError::Vfs(source)
    }
}

impl From<FreelistError> for PagerError {
    fn from(source: FreelistError) -> Self {
        PagerError::Freelist(source)
    }
}
