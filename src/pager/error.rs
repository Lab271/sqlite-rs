use thiserror::Error;

use crate::vfs::{PageError, VfsError};

#[derive(Debug, Error)]
pub enum PagerError {
    #[error(
        "hot rollback journal present at {path}: database was not cleanly closed and its main \
         file may not reflect committed data; refusing to open read-only rather than risk \
         serving pre-rollback pages as committed"
    )]
    HotJournal { path: String },

    #[error(transparent)]
    Page(#[from] PageError),

    #[error(transparent)]
    Vfs(#[from] VfsError),
}
