//! Requirement 1/2 scenarios (spec 006-btree): full-table scans of the
//! `btrees/` fixture family via the real corpus path (`oracle::corpus_dir`).
//! Byte-level correctness (row content, overflow-chain SHA-256 parity) is
//! already proven by `src/btree/mod.rs`'s own inline unit tests against
//! these same fixtures; this file instead proves the cursor integrates
//! correctly through the corpus harness's own fixture-path resolution.

use sqlite_rs::btree::{IndexCursor, TableCursor};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::vfs::{UnixVfs, Vfs, VfsPageSource};

use crate::oracle::corpus_dir;

fn open_header(name: &str) -> (DatabaseHeader, std::path::PathBuf) {
    let path = corpus_dir().join("btrees").join(name);
    let vfs = UnixVfs;
    let file = vfs
        .open_read(&path)
        .unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    (header, path)
}

fn open_cursor(name: &str) -> TableCursor<VfsPageSource> {
    let (header, path) = open_header(name);
    let source = VfsPageSource::open(&UnixVfs, &path, header.page_size).unwrap();
    TableCursor::new(source, &header, 2)
}

fn open_index_cursor(name: &str, root_page: u32) -> IndexCursor<VfsPageSource> {
    let (header, path) = open_header(name);
    let source = VfsPageSource::open(&UnixVfs, &path, header.page_size).unwrap();
    IndexCursor::new(source, header.usable_page_size(), root_page)
}

fn row_count(cursor: &mut TableCursor<VfsPageSource>) -> usize {
    let mut n = 0;
    let mut row = cursor.first().unwrap();
    while row.is_some() {
        n += 1;
        row = cursor.next().unwrap();
    }
    n
}

#[test]
fn table_single_page_row_count() {
    let mut cursor = open_cursor("table_single_page.db");
    assert_eq!(row_count(&mut cursor), 1);
}

#[test]
fn table_multipage_row_count_matches_oracle() {
    let mut cursor = open_cursor("table_multipage.db");
    assert_eq!(row_count(&mut cursor), 3000);
}

#[test]
fn overflow_fixtures_have_exactly_one_row() {
    assert_eq!(row_count(&mut open_cursor("overflow_single_page.db")), 1);
    assert_eq!(row_count(&mut open_cursor("overflow_multi_page.db")), 1);
}

#[test]
fn secondary_index_row_count_matches_oracle() {
    let mut cursor = open_index_cursor("index.db", 3);
    let mut n = 0;
    let mut row = cursor.first().unwrap();
    while row.is_some() {
        n += 1;
        row = cursor.next().unwrap();
    }
    assert_eq!(n, 3000);
}

#[test]
fn without_rowid_row_count_matches_oracle() {
    let mut cursor = open_index_cursor("without_rowid.db", 2);
    let mut n = 0;
    let mut row = cursor.first().unwrap();
    while row.is_some() {
        n += 1;
        row = cursor.next().unwrap();
    }
    assert_eq!(n, 500);
}
