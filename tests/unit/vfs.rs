//! Black-box tests of `sqlite_rs::vfs::*` — only public paths, exactly as an
//! external consumer of the crate would see them.

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
