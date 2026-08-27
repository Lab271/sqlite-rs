// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #169 acceptance: table b-tree delete (cell delete, page merge/collapse
//! on underflow) must produce files stock `sqlite3` opens,
//! `PRAGMA integrity_check`s cleanly, and reads back identically. Follows
//! `btree_insert_test.rs`'s pattern: seed the fixture by shelling out to
//! the oracle directly (`run_oracle` is read-only), then write through
//! `sqlite_rs::btree::{insert_row, delete_row}` and verify via the oracle.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::{delete_row, insert_row, TableCursor};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::record::{encode_record, TextEncoding, Value};
use sqlite_rs::schema::read_schema;
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-btree-delete-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn seed(oracle: &PathBuf, db: &PathBuf, sql: &str) {
    let status = Command::new(oracle).arg(db).arg(sql).status().unwrap();
    assert!(status.success());
}

fn page_size_of(vfs: &UnixVfs, db: &Path) -> u32 {
    let pager = Pager::open(vfs, db, 4096).unwrap();
    let header = pager.read_page(1).unwrap();
    let page_size = u16::from_be_bytes([header[16], header[17]]) as u32;
    if page_size == 1 {
        65536
    } else {
        page_size
    }
}

fn root_page_of(vfs: &UnixVfs, db: &Path, header: &DatabaseHeader, table: &str) -> u32 {
    let pager = Pager::open(vfs, db, header.page_size).unwrap();
    let mut cursor = TableCursor::new(pager, header, 1);
    let schemas = read_schema(&mut cursor, header.text_encoding).unwrap();
    schemas
        .iter()
        .find(|s| s.name == table)
        .unwrap_or_else(|| panic!("table {table} in sqlite_master"))
        .root_page
}

fn read_header(vfs: &UnixVfs, db: &Path, page_size: u32) -> DatabaseHeader {
    let pager = Pager::open(vfs, db, page_size).unwrap();
    let raw = pager.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&raw[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

fn oracle_select(oracle: &PathBuf, db: &PathBuf, sql: &str) -> String {
    let out = Command::new(oracle)
        .arg("-readonly")
        .arg("-list")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn row_payload(b: &str) -> Vec<u8> {
    encode_record(
        &[Value::Null, Value::Text(b.to_string().into())],
        TextEncoding::Utf8,
    )
}

fn row_payload_blob(len: usize) -> Vec<u8> {
    encode_record(
        &[Value::Null, Value::Blob(vec![0xab; len].into())],
        TextEncoding::Utf8,
    )
}

fn insert_rows(pager: &mut Pager, header: &DatabaseHeader, root: u32, rows: &[(i64, String)]) {
    for (rowid, b) in rows {
        insert_row(pager, header, root, *rowid, &row_payload(b)).unwrap();
    }
    pager.flush().unwrap();
}

#[test]
fn delete_single_row_from_a_two_row_leaf() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("delete_single_row_from_a_two_row_leaf");
        return;
    };

    let db = scratch_db("single_row");
    seed(
        &oracle,
        &db,
        "create table t(a integer primary key, b text);",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(
            &mut pager,
            &header,
            root,
            &[(1, "one".to_string()), (2, "two".to_string())],
        );
        delete_row(&mut pager, &header, root, 1).unwrap();
        pager.flush().unwrap();
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "1");
    assert_eq!(oracle_select(&oracle, &db, "select a, b from t;"), "2|two");

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn delete_all_rows_leaves_an_empty_table() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("delete_all_rows_leaves_an_empty_table");
        return;
    };

    let db = scratch_db("delete_all");
    seed(
        &oracle,
        &db,
        "create table t(a integer primary key, b text);",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    let filler = "x".repeat(190);
    let rows: Vec<(i64, String)> = (1..=80).map(|i| (i, format!("{filler}-{i}"))).collect();

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(&mut pager, &header, root, &rows);
        for (rowid, _) in &rows {
            delete_row(&mut pager, &header, root, *rowid).unwrap();
        }
        pager.flush().unwrap();
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "0");

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn bulk_delete_every_other_row_out_of_1000() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("bulk_delete_every_other_row_out_of_1000");
        return;
    };

    let db = scratch_db("bulk_delete");
    seed(
        &oracle,
        &db,
        "create table t(a integer primary key, b text);",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    let rows: Vec<(i64, String)> = (1..=1000).map(|i| (i, format!("row-{i}"))).collect();

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(&mut pager, &header, root, &rows);
        for i in (1..=1000i64).step_by(2) {
            delete_row(&mut pager, &header, root, i).unwrap();
        }
        pager.flush().unwrap();
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(
        oracle_select(&oracle, &db, "select count(*) from t;"),
        "500"
    );
    assert_eq!(
        oracle_select(&oracle, &db, "select sum(a) from t;"),
        (1..=500i64).map(|i| i * 2).sum::<i64>().to_string()
    );
    assert_eq!(
        oracle_select(&oracle, &db, "select b from t where a = 2;"),
        "row-2"
    );
    assert_eq!(
        oracle_select(&oracle, &db, "select count(*) from t where a = 1;"),
        "0"
    );

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn delete_triggers_page_collapse_across_a_split_boundary() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("delete_triggers_page_collapse_across_a_split_boundary");
        return;
    };

    let db = scratch_db("collapse");
    seed(
        &oracle,
        &db,
        "create table t(a integer primary key, b text);",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    // ~200-byte rows: 80 rows forces at least one leaf split (mirrors
    // insert_forces_a_leaf_split). Deleting every row emptied in reverse
    // order exercises the second (rightmost) leaf collapsing away first,
    // then the first leaf collapsing the interior root back to a single
    // empty leaf.
    let filler = "x".repeat(190);
    let rows: Vec<(i64, String)> = (1..=80).map(|i| (i, format!("{filler}-{i}"))).collect();

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(&mut pager, &header, root, &rows);
        for i in (1..=80i64).rev() {
            delete_row(&mut pager, &header, root, i).unwrap();
        }
        pager.flush().unwrap();
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "0");

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn round_trip_insert_delete_insert_reuses_freed_pages() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("round_trip_insert_delete_insert_reuses_freed_pages");
        return;
    };

    let db = scratch_db("round_trip");
    seed(
        &oracle,
        &db,
        "create table t(a integer primary key, b text);",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    let filler = "x".repeat(190);
    let rows: Vec<(i64, String)> = (1..=80).map(|i| (i, format!("{filler}-{i}"))).collect();

    let page_count_after_insert;
    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(&mut pager, &header, root, &rows);
        for (rowid, _) in &rows {
            delete_row(&mut pager, &header, root, *rowid).unwrap();
        }
        pager.flush().unwrap();
        let raw = pager.read_page(1).unwrap();
        page_count_after_insert = u32::from_be_bytes([raw[28], raw[29], raw[30], raw[31]]);
    }

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(&mut pager, &header, root, &rows);
        pager.flush().unwrap();
        let raw = pager.read_page(1).unwrap();
        let page_count_after_reinsert = u32::from_be_bytes([raw[28], raw[29], raw[30], raw[31]]);
        // Reinserting the same rows must reuse pages freed by the prior
        // delete pass via the freelist rather than growing the file further.
        assert_eq!(page_count_after_reinsert, page_count_after_insert);
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "80");

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn deleting_a_row_with_an_overflow_payload_frees_its_overflow_pages() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("deleting_a_row_with_an_overflow_payload_frees_its_overflow_pages");
        return;
    };

    let db = scratch_db("overflow_delete");
    seed(
        &oracle,
        &db,
        "create table t(a integer primary key, b blob);",
    );

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    // A 10KB blob is far larger than any page-size-derived local-payload
    // max, so this row spills into a multi-page overflow chain (#173).
    let blob = row_payload_blob(10_000);

    let page_count_after_insert;
    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_row(&mut pager, &header, root, 1, &blob).unwrap();
        insert_row(&mut pager, &header, root, 2, &row_payload("kept")).unwrap();
        pager.flush().unwrap();

        delete_row(&mut pager, &header, root, 1).unwrap();
        pager.flush().unwrap();
        let raw = pager.read_page(1).unwrap();
        page_count_after_insert = u32::from_be_bytes([raw[28], raw[29], raw[30], raw[31]]);
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "1");
    assert_eq!(oracle_select(&oracle, &db, "select a from t;"), "2");

    // Re-inserting a same-sized overflow row must reuse the freelist pages
    // the deleted row's overflow chain returned rather than growing the
    // file — the acceptance criterion this test exists to cover.
    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_row(&mut pager, &header, root, 3, &blob).unwrap();
        pager.flush().unwrap();
        let raw = pager.read_page(1).unwrap();
        let page_count_after_reinsert = u32::from_be_bytes([raw[28], raw[29], raw[30], raw[31]]);
        assert_eq!(page_count_after_reinsert, page_count_after_insert);
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "2");

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}
