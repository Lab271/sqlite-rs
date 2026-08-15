// `deny`, not `forbid`: `src/vfs/lock.rs` needs a scoped
// `#![allow(unsafe_code)]` for the raw `fcntl` calls behind journal-mode
// SHARED locking (#50) — `forbid` can never be locally overridden, `deny`
// can.
#![deny(unsafe_code)]

pub mod btree;
pub mod dump;
pub mod format;
pub mod header;
pub mod pager;
pub mod parser;
pub mod record;
pub mod schema;
pub mod vfs;
