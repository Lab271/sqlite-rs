//! #196 acceptance: `compile_insert`/`compile_delete`/`compile_update`
//! keep secondary indexes in sync with table data. Seeds a table +
//! index via the oracle (so index root pages exist), reads its
//! `TableSchema` (#211's `indexes` catalog) via `read_schema`, then runs
//! our own codegen through `execute_with_writable_db` and checks the
//! oracle's own `PRAGMA integrity_check` — which specifically validates
//! that every index has exactly the row set its table does — plus an
//! indexed `SELECT` to confirm the index is actually usable for lookups
//! (a corrupt/stale index that integrity_check somehow missed would
//! still answer these wrong).

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::update::compile_update;
use sqlite_rs::codegen::{compile_delete, compile_insert};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::ast::Update;
use sqlite_rs::parser::{
    parse_delete, parse_insert, parse_update, DeleteOutcome, InsertOutcome, ParseOutcome,
};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::execute_with_writable_db;
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-index-maintenance-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn seed(oracle: &PathBuf, db: &PathBuf, sql: &str) {
    let status = Command::new(oracle).arg(db).arg(sql).status().unwrap();
    assert!(status.success());
}

fn page_size_of(db: &PathBuf) -> u32 {
    let vfs = UnixVfs;
    let file = vfs.open_read(db).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let page_size = u16::from_be_bytes([header_buf[16], header_buf[17]]) as u32;
    if page_size == 1 {
        65536
    } else {
        page_size
    }
}

fn read_header(db: &PathBuf, page_size: u32) -> DatabaseHeader {
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, db, page_size).unwrap();
    let raw = pager.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&raw[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

fn table_schema(db: &PathBuf, header: &DatabaseHeader, table: &str) -> TableSchema {
    let vfs = UnixVfs;
    let source = VfsPageSource::open(&vfs, db, header.page_size).unwrap();
    let mut cursor = TableCursor::new(source, header, 1);
    let schemas = read_schema(&mut cursor, header.text_encoding).unwrap();
    schemas
        .into_iter()
        .find(|s| s.name == table)
        .unwrap_or_else(|| panic!("no schema for table {table}"))
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

#[test]
fn insert_maintains_a_secondary_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("insert");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_b ON t(b); \
         INSERT INTO t VALUES (1, 'seed');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes.len(), 1);

    let insert = match parse_insert("INSERT INTO t VALUES (2, 'apple'), (3, 'banana')") {
        InsertOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_b WHERE b = 'apple'"
        ),
        "2"
    );
}

#[test]
fn delete_maintains_a_secondary_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("delete");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_b ON t(b); \
         INSERT INTO t VALUES (1, 'apple'), (2, 'banana'), (3, 'cherry');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let delete = match parse_delete("DELETE FROM t WHERE b = 'banana'") {
        DeleteOutcome::Accepted(d) => *d,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_delete(&delete, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT count(*) FROM t INDEXED BY idx_b WHERE b = 'banana'"
        ),
        "0"
    );
}

#[test]
fn update_maintains_a_secondary_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("update");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_b ON t(b); \
         INSERT INTO t VALUES (1, 'apple'), (2, 'banana');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let update: Update = match parse_update("UPDATE t SET b = 'zebra' WHERE id = 2") {
        ParseOutcome::Accepted(u) => *u,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_update(&update, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_b WHERE b = 'zebra'"
        ),
        "2"
    );
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT count(*) FROM t INDEXED BY idx_b WHERE b = 'banana'"
        ),
        "0"
    );
}
