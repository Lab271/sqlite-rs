//! Black-box tests of `sqlite_rs::header::*` — only public paths, exactly as
//! an external consumer of the crate would see them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::path::Path;

use sqlite_rs::header::{DatabaseHeader, HeaderError, JournalMode, VersionField, HEADER_LEN};

fn fixture(family: &str, name: &str) -> Vec<u8> {
    let path = Path::new("tests/corpus/fixtures").join(family).join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading fixture {path:?}: {e}"))
}

#[test]
fn parses_a_valid_header() {
    let header = DatabaseHeader::parse(&fixture("pagesizes", "reserved_bytes_0.db")).unwrap();
    assert_eq!(header.page_size, 4096);
    assert_eq!(header.journal_mode(), JournalMode::Legacy);
    assert_eq!(header.usable_page_size(), 4096);
}

#[test]
fn header_error_variants_are_matchable() {
    // Consumers must be able to `match` on every variant by name — this
    // would fail to compile if a variant's fields were made private/renamed.
    let err = DatabaseHeader::parse(&fixture("invalid", "empty.db")).unwrap_err();
    match err {
        HeaderError::TooShort { len } => assert_eq!(len, 0),
        other => panic!("expected TooShort, got {other:?}"),
    }

    let err = DatabaseHeader::parse(&fixture("invalid", "magic.db")).unwrap_err();
    assert!(matches!(err, HeaderError::InvalidMagic));

    let mut bytes = fixture("pagesizes", "page_size_512.db");
    bytes[18] = 9;
    let err = DatabaseHeader::parse(&bytes).unwrap_err();
    match err {
        HeaderError::InvalidFileFormatVersion { field, value } => {
            assert_eq!(field, VersionField::Write);
            assert_eq!(value, 9);
        }
        other => panic!("expected InvalidFileFormatVersion, got {other:?}"),
    }
}

#[test]
fn header_error_is_error_send_sync() {
    fn assert_bounds<T: std::error::Error + Send + Sync + 'static>() {}
    assert_bounds::<HeaderError>();
}

#[test]
fn header_len_constant_is_100() {
    assert_eq!(HEADER_LEN, 100);
}

/// Spec 003/Req-2 "Page-1 offset documentation" scenario: the 100-byte
/// header occupies the start of page 1, but page 1's b-tree cell-pointer
/// array is relative to byte 0 of the page, not byte 100. `DatabaseHeader`
/// parses only the header; a page-1 buffer must remain addressable from
/// byte 0 by any code layered on top (the b-tree layer), not from
/// `HEADER_LEN`.
#[test]
fn page_1_header_does_not_shift_page_relative_offsets() {
    let page1 = fixture("pagesizes", "reserved_bytes_0.db");
    let header = DatabaseHeader::parse(&page1).unwrap();

    // The header itself is exactly HEADER_LEN bytes, addressed from byte 0.
    assert_eq!(HEADER_LEN, 100);
    assert!(page1.len() >= header.page_size as usize);

    // Byte 100 (first byte after the header) is where page-1's own
    // in-page content — not a second copy of the header — begins.
    // Re-parsing a header at that offset must fail: it is not header bytes.
    assert!(DatabaseHeader::parse(&page1[HEADER_LEN..]).is_err());

    // The page buffer as a whole is still addressed relative to byte 0,
    // not byte 100 — i.e. `page1[0..HEADER_LEN]` is the header, and
    // `page1` itself (not a sub-slice starting at HEADER_LEN) is what a
    // page-1-aware offset calculation must index into.
    assert_eq!(&page1[0..16], b"SQLite format 3\0");
}
