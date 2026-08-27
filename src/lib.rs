// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! A binary-compatible Rust replication of SQLite: read (and, for WAL,
//! write) the same on-disk file format and SQL dialect as the C library,
//! targeting a memory-safe, extensible SQLite rather than a new engine.
//! See the [repository README](https://github.com/iheitlager/sqlite-rs)
//! for the full design rationale.
//!
//! `include_str!` can't pull the README in directly here: `src/` is a
//! qualified Rust subset checked by `make mvl-limit` (mvl-rust rust-limit),
//! which doesn't allowlist that macro.
// `src/vfs/lock.rs` and `src/vfs/shm.rs` used to need a scoped
// `#![allow(unsafe_code)]` for raw `fcntl`/`mmap`/`fork` calls (#50), then
// went unsafe-free entirely under `nix`/`std` (#66). Vendoring `nix`'s
// `fcntl`/`termios` FFI (#563) reintroduces one, deliberately narrow,
// carve-out: `src/sys/` — see `.openspec/adr/0031-vendor-nix-subset.md`.
// `deny` (rather than `forbid`) is what makes that local
// `#![allow(unsafe_code)]` possible; every other module is still held to
// zero `unsafe` by this crate-wide default.
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod btree;
pub mod codegen;
pub mod dump;
pub mod format;
pub mod header;
pub mod integrity;
pub mod pager;
pub mod parser;
pub mod planner;
pub mod record;
pub mod schema;
pub mod sys;
pub mod vdbe;
pub mod vfs;
