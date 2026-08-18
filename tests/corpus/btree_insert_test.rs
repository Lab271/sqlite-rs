//! #168 acceptance: table b-tree insert (cell insert, leaf split,
//! cascading interior splits, root split) must produce files stock
//! `sqlite3` opens, `PRAGMA integrity_check`s cleanly, and reads back
//! identically. Follows `pager_write_test.rs`'s pattern: seed the fixture
//! by shelling out to the oracle directly (`run_oracle` is read-only), then
//! write through `sqlite_rs::btree::insert_row` and verify via the oracle.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::{insert_row, TableCursor};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::record::{encode_record, TextEncoding, Value};
use sqlite_rs::schema::read_schema;
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-btree-insert-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn seed(oracle: &PathBuf, db: &PathBuf, sql: &str) {
    let status = Command::new(oracle).arg(db).arg(sql).status().unwrap();
    assert!(status.success());
}

fn page_size_of(vfs: &UnixVfs, db: &PathBuf) -> u32 {
    let pager = Pager::open(vfs, db, 4096).unwrap();
    let header = pager.read_page(1).unwrap();
    let page_size = u16::from_be_bytes([header[16], header[17]]) as u32;
    if page_size == 1 {
        65536
    } else {
        page_size
    }
}

fn root_page_of(vfs: &UnixVfs, db: &PathBuf, header: &DatabaseHeader, table: &str) -> u32 {
    let pager = Pager::open(vfs, db, header.page_size).unwrap();
    let mut cursor = TableCursor::new(pager, header, 1);
    let schemas = read_schema(&mut cursor, header.text_encoding).unwrap();
    schemas
        .iter()
        .find(|s| s.name == table)
        .unwrap_or_else(|| panic!("table {table} in sqlite_master"))
        .root_page
}

fn read_header(vfs: &UnixVfs, db: &PathBuf, page_size: u32) -> DatabaseHeader {
    let pager = Pager::open(vfs, db, page_size).unwrap();
    let raw = pager.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&raw[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

fn assert_integrity_ok(oracle: &PathBuf, db: &PathBuf) {
    let integrity = Command::new(oracle)
        .arg("-readonly")
        .arg(db)
        .arg("PRAGMA integrity_check;")
        .output()
        .unwrap();
    assert!(integrity.status.success());
    assert_eq!(String::from_utf8_lossy(&integrity.stdout).trim(), "ok");
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

/// Encodes a `(a, b)` row's record body as `t(a INTEGER, b TEXT)` with `a`
/// as the rowid-alias column (stored as NULL, per the rowid-alias
/// convention `src/btree.rs` documents on the read side).
fn row_payload(b: &str) -> Vec<u8> {
    encode_record(&[Value::Null, Value::Text(b.to_string())], TextEncoding::Utf8)
}

/// Inserts `(rowid, b)` for every row into `t`'s root, flushing after each
/// insert so every step is a fully committed page state.
fn insert_rows(pager: &mut Pager, header: &DatabaseHeader, root: u32, rows: &[(i64, String)]) {
    for (rowid, b) in rows {
        insert_row(pager, header, root, *rowid, &row_payload(b)).unwrap();
    }
    pager.flush().unwrap();
}

#[test]
fn insert_single_row_no_split() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("insert_single_row_no_split");
        return;
    };

    let db = scratch_db("no_split");
    seed(&oracle, &db, "create table t(a integer primary key, b text);");

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(&mut pager, &header, root, &[(1, "one".to_string())]);
    }

    assert_integrity_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select a, b from t;"), "1|one");

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn insert_forces_a_leaf_split() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("insert_forces_a_leaf_split");
        return;
    };

    let db = scratch_db("leaf_split");
    seed(&oracle, &db, "create table t(a integer primary key, b text);");

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    // ~200-byte rows: a 4096-byte page holds well under 100 of these, so
    // 80 rows forces at least one leaf split while staying inside a single
    // interior level (no cascading/root split — that's covered below).
    let filler = "x".repeat(190);
    let rows: Vec<(i64, String)> = (1..=80).map(|i| (i, format!("{filler}-{i}"))).collect();

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(&mut pager, &header, root, &rows);
    }

    assert_integrity_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "80");
    assert_eq!(
        oracle_select(&oracle, &db, "select b from t where a = 1;"),
        format!("{filler}-1")
    );
    assert_eq!(
        oracle_select(&oracle, &db, "select b from t where a = 80;"),
        format!("{filler}-80")
    );

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn insert_forces_cascading_splits_and_a_root_split() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("insert_forces_cascading_splits_and_a_root_split");
        return;
    };

    let db = scratch_db("root_split");
    seed(&oracle, &db, "create table t(a integer primary key, b text);");

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    // Enough ~200-byte rows to overflow one leaf level's worth of pages
    // several times over, forcing the root (a leaf at row 1) to split into
    // an interior root with cascading child splits underneath.
    let filler = "y".repeat(190);
    let rows: Vec<(i64, String)> = (1..=2000).map(|i| (i, format!("{filler}-{i}"))).collect();

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(&mut pager, &header, root, &rows);
    }

    assert_integrity_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "2000");
    assert_eq!(
        oracle_select(&oracle, &db, "select b from t where a = 1;"),
        format!("{filler}-1")
    );
    assert_eq!(
        oracle_select(&oracle, &db, "select b from t where a = 2000;"),
        format!("{filler}-2000")
    );
    assert_eq!(
        oracle_select(&oracle, &db, "select b from t where a = 1000;"),
        format!("{filler}-1000")
    );

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn bulk_insert_1000_rows_is_oracle_identical() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("bulk_insert_1000_rows_is_oracle_identical");
        return;
    };

    let db = scratch_db("bulk_1000");
    seed(&oracle, &db, "create table t(a integer primary key, b text);");

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    let rows: Vec<(i64, String)> = (1..=1000).map(|i| (i, format!("row-{i}"))).collect();

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(&mut pager, &header, root, &rows);
    }

    assert_integrity_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "1000");
    assert_eq!(
        oracle_select(&oracle, &db, "select sum(a) from t;"),
        (1..=1000i64).sum::<i64>().to_string()
    );

    let expected: String = rows
        .iter()
        .map(|(a, b)| format!("{a}|{b}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(oracle_select(&oracle, &db, "select a, b from t order by a;"), expected);

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

#[test]
fn insert_with_overflow_payload_combined_with_a_split() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("insert_with_overflow_payload_combined_with_a_split");
        return;
    };

    let db = scratch_db("overflow_split");
    seed(&oracle, &db, "create table t(a integer primary key, b text);");

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);
    let root = root_page_of(&vfs, &db, &header, "t");

    // A payload well past a 4096-byte page's max-local-size (usable_size -
    // 35) forces an overflow chain; enough of these force a leaf split too.
    let big = "z".repeat(10_000);
    let rows: Vec<(i64, String)> = (1..=10).map(|i| (i, format!("{big}-{i}"))).collect();

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        insert_rows(&mut pager, &header, root, &rows);
    }

    assert_integrity_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "select count(*) from t;"), "10");
    assert_eq!(
        oracle_select(&oracle, &db, "select length(b) from t where a = 1;"),
        (big.len() + 2).to_string()
    );
    assert_eq!(
        oracle_select(&oracle, &db, "select b from t where a = 10;"),
        format!("{big}-10")
    );

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

/// `sqlite_master` (root page 1) is the one table b-tree whose root
/// physically shares its page with the 100-byte file header (the
/// "page-1 trap"). Inserting directly into it, enough to force page 1
/// itself to split into an interior root, pins that every leaf/interior
/// rewrite this module makes to page 1 preserves bytes 0..100 — a real
/// bug caught during development (`write_page_common` originally zeroed
/// the whole page, wiping the file header).
#[test]
fn insert_into_page_one_root_preserves_the_file_header_across_a_split() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("insert_into_page_one_root_preserves_the_file_header_across_a_split");
        return;
    };

    let db = scratch_db("page1_root");
    seed(&oracle, &db, "create table t(a integer primary key, b text);");

    let vfs = UnixVfs;
    let page_size = page_size_of(&vfs, &db);
    let header = read_header(&vfs, &db, page_size);

    // Trigger rows (rootpage 0, real trigger DDL referencing `t`) are
    // valid `sqlite_master` entries stock `sqlite3` accepts on schema
    // load, so they're safe filler to force enough leaf splits that page
    // 1 itself must split into an interior root — unlike a `table`-type
    // entry, which schema validation rejects unless it points at a real
    // b-tree root.
    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        for i in 0..80i64 {
            let rowid = 1000 + i;
            let name = format!("trg{i}");
            let sql = format!("CREATE TRIGGER {name} AFTER INSERT ON t BEGIN SELECT 1; END");
            let payload = encode_record(
                &[
                    Value::Text("trigger".to_string()),
                    Value::Text(name.clone()),
                    Value::Text("t".to_string()),
                    Value::Integer(0),
                    Value::Text(sql),
                ],
                TextEncoding::Utf8,
            );
            insert_row(&mut pager, &header, 1, rowid, &payload).unwrap();
        }
        pager.flush().unwrap();
    }

    assert_integrity_ok(&oracle, &db);
    // The original `t` entry, seeded before any insert, must have
    // survived every page-1 rewrite unchanged.
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "select sql from sqlite_master where type = 'table' and tbl_name = 't';"
        ),
        "CREATE TABLE t(a integer primary key, b text)"
    );
    assert_eq!(
        oracle_select(&oracle, &db, "select count(*) from sqlite_master;"),
        "81"
    );

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}
