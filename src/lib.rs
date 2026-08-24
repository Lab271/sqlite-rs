//! A binary-compatible Rust replication of SQLite: read (and, for WAL,
//! write) the same on-disk file format and SQL dialect as the C library,
//! targeting a memory-safe, extensible SQLite rather than a new engine.
//! See the [repository README](https://github.com/iheitlager/sqlite-rs)
//! for the full design rationale.
//!
//! `include_str!` can't pull the README in directly here: `src/` is a
//! qualified Rust subset checked by `make mvl-limit` (mvl-rust rust-limit),
//! which doesn't allowlist that macro.
// Crate-wide, with no local override possible: `src/vfs/lock.rs` and
// `src/vfs/shm.rs` used to need a scoped `#![allow(unsafe_code)]` for raw
// `fcntl`/`mmap`/`fork` calls (#50); both are now safe `nix`/`std` APIs
// (#66), so nothing in this crate needs `unsafe` anymore.
#![forbid(unsafe_code)]

pub mod btree;
pub mod codegen;
pub mod dump;
pub mod format;
pub mod header;
pub mod pager;
pub mod parser;
pub mod planner;
pub mod record;
pub mod schema;
pub mod vdbe;
pub mod vfs;
