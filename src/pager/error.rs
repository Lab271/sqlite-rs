use thiserror::Error;

use super::freelist::FreelistError;
use super::journal::JournalError;
use super::wal::WalError;
use crate::vfs::{PageError, VfsError};

/// Errors surfaced by the pager while opening a database, replaying
/// recovery state, or servicing page reads/writes.
#[derive(Debug, Error)]
pub enum PagerError {
    /// A hot rollback journal exists at `path`: the previous writer did not
    /// clean up, so the main database file may not reflect committed data.
    #[error(
        "hot rollback journal present at {path}: database was not cleanly closed and its main \
         file may not reflect committed data; refusing to open read-only rather than risk \
         serving pre-rollback pages as committed"
    )]
    HotJournal {
        /// Path to the hot journal file that triggered the refusal.
        path: String,
    },

    /// The rollback journal itself failed to parse.
    #[error("rollback journal is corrupt: {0}")]
    Journal(#[source] JournalError),

    /// The write-ahead log at `path` failed to parse or validate.
    #[error("reading WAL at {path}: {source}")]
    Wal {
        /// Path to the WAL file that failed.
        path: String,
        /// The underlying WAL parsing/validation error.
        #[source]
        source: WalError,
    },

    /// A page-level error propagated from the storage layer.
    #[error(transparent)]
    Page(#[from] PageError),

    /// A VFS-level I/O or locking error.
    #[error(transparent)]
    Vfs(#[from] VfsError),

    /// A freelist trunk/leaf page failed to parse.
    #[error(transparent)]
    Freelist(#[from] FreelistError),

    /// `journal_mode` cannot be changed while a transaction is pending.
    #[error("cannot change journal_mode with a pending transaction")]
    PendingTransaction,

    /// Switching `journal_mode` out of WAL requires a checkpoint that fully
    /// back-fills the WAL into the main file first; the checkpoint left
    /// frames behind.
    #[error("checkpoint did not fully back-fill the WAL while switching journal_mode out of WAL")]
    CheckpointIncomplete,
}
