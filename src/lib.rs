// Crate-wide, with no local override possible: `src/vfs/lock.rs` and
// `src/vfs/shm.rs` used to need a scoped `#![allow(unsafe_code)]` for raw
// `fcntl`/`mmap`/`fork` calls (#50); both are now safe `nix`/`std` APIs
// (#66), so nothing in this crate needs `unsafe` anymore.
#![forbid(unsafe_code)]

pub mod btree;
pub mod dump;
pub mod format;
pub mod header;
pub mod pager;
pub mod parser;
pub mod record;
pub mod schema;
pub mod vdbe;
pub mod vfs;
