// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Black-box tests of `sqlite_rs::vfs::*` — only public paths, exactly as an
//! external consumer of the crate would see them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::path::Path;

use sqlite_rs::vfs::{companion_path, MemoryVfs, Vfs, VfsError};

#[test]
fn memory_vfs_read_roundtrip() {
    let mut vfs = MemoryVfs::new();
    let contents = b"hello from a public-api test".to_vec();
    vfs.insert("/present.db", contents.clone());

    assert!(vfs.exists(Path::new("/present.db")).unwrap());
    let file = vfs.open_read(Path::new("/present.db")).unwrap();
    assert_eq!(file.size().unwrap(), contents.len() as u64);

    let mut buf = vec![0u8; contents.len()];
    let n = file.read_at(&mut buf, 0).unwrap();
    assert_eq!(n, contents.len());
    assert_eq!(buf, contents);
}

/// Spec 003/Req-1 "Companion file detection" scenario: sibling `-wal` /
/// `-journal` files are addressed by appending a suffix to the main file's
/// full name, not by substituting its extension.
#[test]
fn companion_file_detection_from_public_api() {
    let mut vfs = MemoryVfs::new();
    vfs.insert("/test.db", b"main file".to_vec());
    vfs.insert("/test.db-wal", b"wal file".to_vec());
    vfs.insert("/test.db-journal", b"journal file".to_vec());

    assert!(vfs
        .exists(&companion_path(Path::new("/test.db"), "-wal"))
        .unwrap());
    assert!(vfs
        .exists(&companion_path(Path::new("/test.db"), "-journal"))
        .unwrap());
    assert!(!vfs
        .exists(&companion_path(Path::new("/other.db"), "-wal"))
        .unwrap());

    // Appended after the full name, not substituted for the extension.
    assert_eq!(
        companion_path(Path::new("/test.db"), "-wal"),
        Path::new("/test.db-wal")
    );
}

/// #172 rollback journal: `create_or_open_write` must create a missing
/// file (not error like `open_write`), and a clone of the `Vfs` handle
/// must see it too — `Pager` stores its own `Clone` of the `Vfs` it was
/// opened with to create/delete the `-journal` file after `open` returns.
#[test]
fn create_or_open_write_creates_missing_file_visible_to_clone() {
    let vfs = MemoryVfs::new();
    let path = Path::new("/new.db-journal");
    assert!(!vfs.exists(path).unwrap());

    let clone = vfs.clone();
    let file = clone.create_or_open_write(path).unwrap();
    file.write_at(b"header", 0).unwrap();

    assert!(vfs.exists(path).unwrap());
    let reopened = vfs.open_read(path).unwrap();
    let mut buf = [0u8; 6];
    reopened.read_at(&mut buf, 0).unwrap();
    assert_eq!(&buf, b"header");
}

/// `delete` removes the file, and is a no-op (not an error) when the file
/// is already absent — matching `std::fs::remove_file`'s `NotFound` case
/// being folded into `Ok(())` for the real `UnixVfs` backend too, since
/// commit's journal-delete step must not fail if the journal was already
/// cleaned up by a previous, partially-completed commit.
#[test]
fn delete_removes_file_and_is_a_noop_when_absent() {
    let mut vfs = MemoryVfs::new();
    vfs.insert("/test.db-journal", b"stale journal".to_vec());
    let path = Path::new("/test.db-journal");

    vfs.delete(path).unwrap();
    assert!(!vfs.exists(path).unwrap());

    // Deleting again (already absent) is still Ok.
    vfs.delete(path).unwrap();
}

#[test]
fn vfs_error_variants_are_matchable() {
    let vfs = MemoryVfs::new();
    let err = match vfs.open_read(Path::new("/missing.db")) {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    };
    match err {
        VfsError::NotFound { path } => assert_eq!(path, "/missing.db"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn vfs_error_is_error_send_sync() {
    fn assert_bounds<T: std::error::Error + Send + Sync + 'static>() {}
    assert_bounds::<VfsError>();
}
