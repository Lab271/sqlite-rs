#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! #195 acceptance: `compile_insert` against a real on-disk table,
//! covering NOT NULL, PRIMARY KEY/rowid conflicts (+ `ON CONFLICT`),
//! CHECK, and DEFAULT — executed via `execute_with_writable_db` and
//! read back with `TableCursor`/`decode_record` (V1's own reader),
//! mirroring `tests/vdbe/write_opcodes_test.rs`'s harness.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_insert;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::{parse_insert, ParseOutcome};
use sqlite_rs::record::{decode_record, TextEncoding, Value};
use sqlite_rs::schema::TableSchema;
use sqlite_rs::vdbe::{execute_with_writable_db, ExecError};
use sqlite_rs::vfs::{UnixVfs, Vfs};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-codegen-insert-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

/// Same one-page-doubles-as-empty-leaf-root simplification
/// `tests/vdbe/write_opcodes_test.rs` uses.
fn seed_minimal_db(vfs: &UnixVfs, path: &Path, page_size: u32) -> DatabaseHeader {
    let mut page1 = vec![0u8; page_size as usize];
    page1[0..16].copy_from_slice(b"SQLite format 3\0");
    page1[16..18].copy_from_slice(&u16::try_from(page_size).unwrap_or(1).to_be_bytes());
    page1[18] = 1;
    page1[19] = 1;
    page1[28..32].copy_from_slice(&1u32.to_be_bytes());
    page1[56..60].copy_from_slice(&1u32.to_be_bytes());

    let header_start = 100usize;
    page1[header_start] = 0x0d; // LEAF_TABLE
    page1[header_start + 1..header_start + 3].copy_from_slice(&0u16.to_be_bytes());
    page1[header_start + 3..header_start + 5].copy_from_slice(&0u16.to_be_bytes());
    let content_start = if page_size == 65536 {
        0u16
    } else {
        u16::try_from(page_size).unwrap()
    };
    page1[header_start + 5..header_start + 7].copy_from_slice(&content_start.to_be_bytes());
    page1[header_start + 7] = 0;

    let file = vfs.create_or_open_write(path).unwrap();
    file.write_at(&page1, 0).unwrap();
    file.sync().unwrap();

    let mut header_buf = [0u8; 100];
    header_buf.copy_from_slice(&page1[..100]);
    DatabaseHeader::parse(&header_buf).unwrap()
}

fn run_insert(
    path: &Path,
    header: &DatabaseHeader,
    page_size: u32,
    sql: &str,
    schema: &TableSchema,
) -> Result<(), ExecError> {
    let insert = match parse_insert(sql) {
        ParseOutcome::Accepted(i) => *i,
        other => panic!("failed to parse {sql:?}: {other:?}"),
    };
    let program = compile_insert(&insert, schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, path, page_size).unwrap();
    execute_with_writable_db(&program, pager, *header).map(|_| ())
}

fn rows(
    path: &Path,
    header: &DatabaseHeader,
    page_size: u32,
    root_page: u32,
) -> Vec<(i64, Vec<Value>)> {
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, path, page_size).unwrap();
    let mut cursor = TableCursor::new(pager, header, root_page);
    let mut out = Vec::new();
    let mut row = cursor.first().unwrap();
    while let Some(r) = row {
        let values = decode_record(&r.payload, TextEncoding::Utf8).unwrap();
        out.push((r.rowid, values));
        row = cursor.next().unwrap();
    }
    out
}

#[test]
fn not_null_violation_halts_and_inserts_nothing() {
    let path = scratch_db("notnull");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let sql = "CREATE TABLE t(a INTEGER NOT NULL, b TEXT)";
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 1,
        columns: vec!["a".to_string(), "b".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
    };

    let err = run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(a, b) VALUES (NULL, 'x')",
        &schema,
    )
    .expect_err("NULL into a NOT NULL column must fail");
    match err {
        ExecError::Halted { code, .. } => assert_eq!(code, 1299),
        other => panic!("expected Halted, got {other:?}"),
    }
    assert!(rows(&path, &header, page_size, 1).is_empty());
}

#[test]
fn valid_row_round_trips() {
    let path = scratch_db("valid-row");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let sql = "CREATE TABLE t(a INTEGER NOT NULL, b TEXT)";
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 1,
        columns: vec!["a".to_string(), "b".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
    };

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(a, b) VALUES (42, 'hi')",
        &schema,
    )
    .unwrap();

    let got = rows(&path, &header, page_size, 1);
    assert_eq!(
        got,
        vec![(1, vec![Value::Integer(42), Value::Text("hi".to_string())])]
    );
}

#[test]
fn default_value_applied_when_column_omitted() {
    let path = scratch_db("default");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let sql = "CREATE TABLE t(a INTEGER, b TEXT DEFAULT 'fallback')";
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 1,
        columns: vec!["a".to_string(), "b".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
    };

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(a) VALUES (1)",
        &schema,
    )
    .unwrap();

    let got = rows(&path, &header, page_size, 1);
    assert_eq!(
        got,
        vec![(
            1,
            vec![Value::Integer(1), Value::Text("fallback".to_string())]
        )]
    );
}

#[test]
fn check_violation_halts() {
    let path = scratch_db("check");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let sql = "CREATE TABLE t(a INTEGER CHECK (a > 0))";
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 1,
        columns: vec!["a".to_string()],
        column_types: vec!["INTEGER".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
    };

    let err = run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(a) VALUES (-1)",
        &schema,
    )
    .expect_err("a negative value must fail the CHECK constraint");
    match err {
        ExecError::Halted { code, .. } => assert_eq!(code, 275),
        other => panic!("expected Halted, got {other:?}"),
    }

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(a) VALUES (5)",
        &schema,
    )
    .unwrap();
    assert_eq!(
        rows(&path, &header, page_size, 1),
        vec![(1, vec![Value::Integer(5)])]
    );
}

#[test]
fn primary_key_conflict_aborts_by_default() {
    let path = scratch_db("pk-abort");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let sql = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)";
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 1,
        columns: vec!["id".to_string(), "v".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
    };

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(id, v) VALUES (1, 'a')",
        &schema,
    )
    .unwrap();
    let err = run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(id, v) VALUES (1, 'b')",
        &schema,
    )
    .expect_err("duplicate PRIMARY KEY must fail");
    match err {
        ExecError::Halted { code, .. } => assert_eq!(code, 1555),
        other => panic!("expected Halted, got {other:?}"),
    }
    assert_eq!(
        rows(&path, &header, page_size, 1),
        vec![(1, vec![Value::Null, Value::Text("a".to_string())])]
    );
}

#[test]
fn primary_key_conflict_or_ignore_skips_the_row() {
    let path = scratch_db("pk-ignore");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let sql = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)";
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 1,
        columns: vec!["id".to_string(), "v".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
    };

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(id, v) VALUES (1, 'a')",
        &schema,
    )
    .unwrap();
    run_insert(
        &path,
        &header,
        page_size,
        "INSERT OR IGNORE INTO t(id, v) VALUES (1, 'b')",
        &schema,
    )
    .unwrap();

    assert_eq!(
        rows(&path, &header, page_size, 1),
        vec![(1, vec![Value::Null, Value::Text("a".to_string())])]
    );
}

#[test]
fn primary_key_conflict_or_replace_overwrites_the_row() {
    let path = scratch_db("pk-replace");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let sql = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)";
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 1,
        columns: vec!["id".to_string(), "v".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
    };

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(id, v) VALUES (1, 'a')",
        &schema,
    )
    .unwrap();
    run_insert(
        &path,
        &header,
        page_size,
        "INSERT OR REPLACE INTO t(id, v) VALUES (1, 'b')",
        &schema,
    )
    .unwrap();

    assert_eq!(
        rows(&path, &header, page_size, 1),
        vec![(1, vec![Value::Null, Value::Text("b".to_string())])]
    );
}

#[test]
fn omitted_rowid_alias_is_auto_assigned() {
    let path = scratch_db("pk-auto");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let sql = "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)";
    let schema = TableSchema {
        name: "t".to_string(),
        root_page: 1,
        columns: vec!["id".to_string(), "v".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
    };

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(v) VALUES ('a')",
        &schema,
    )
    .unwrap();
    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(v) VALUES ('b')",
        &schema,
    )
    .unwrap();

    assert_eq!(
        rows(&path, &header, page_size, 1),
        vec![
            (1, vec![Value::Null, Value::Text("a".to_string())]),
            (2, vec![Value::Null, Value::Text("b".to_string())]),
        ]
    );
}
