use thiserror::Error;

use super::freelist::FreelistError;
use super::journal::JournalError;
use super::wal::WalError;
use crate::vfs::{PageError, VfsError};

#[derive(Debug, Error)]
pub enum PagerError {
    #[error(
        "hot rollback journal present at {path}: database was not cleanly closed and its main \
         file may not reflect committed data; refusing to open read-only rather than risk \
         serving pre-rollback pages as committed"
    )]
    HotJournal { path: String },

    #[error("rollback journal is corrupt: {0}")]
    Journal(#[source] JournalError),

    #[error("reading WAL at {path}: {source}")]
    Wal {
        path: String,
        #[source]
        source: WalError,
    },

    #[error(transparent)]
    Page(#[from] PageError),

    #[error(transparent)]
    Vfs(#[from] VfsError),

    #[error(transparent)]
    Freelist(#[from] FreelistError),

    #[error("cannot change journal_mode with a pending transaction")]
    PendingTransaction,

    #[error("checkpoint did not fully back-fill the WAL while switching journal_mode out of WAL")]
    CheckpointIncomplete,
}
