//! #444 acceptance: covering-index scan and index-only `COUNT(*)`
//! compile to `SeekIndexEq` + `Column` reads that never seek/decode
//! the table row, and produce byte-for-byte the same output as the
//! pinned oracle. Same scratch-db-plus-oracle pattern
//! `index_ordered_scan_test.rs` uses.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_select;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, Opcode};
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-no-stats-opt-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn seed(oracle: &PathBuf, db: &PathBuf, sql: &str) {
    let status = Command::new(oracle).arg(db).arg(sql).status().unwrap();
    assert!(status.success());
}

fn page_size_of(db: &Path) -> u32 {
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

fn read_header(db: &Path, page_size: u32) -> DatabaseHeader {
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, db, page_size).unwrap();
    let raw = pager.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&raw[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

fn table_schema(db: &Path, header: &DatabaseHeader, table: &str) -> TableSchema {
    let vfs = UnixVfs;
    let source = VfsPageSource::open(&vfs, db, header.page_size).unwrap();
    let mut cursor = TableCursor::new(source, header, 1);
    let schemas = read_schema(&mut cursor, header.text_encoding).unwrap();
    schemas
        .into_iter()
        .find(|s| s.name == table)
        .unwrap_or_else(|| panic!("no schema for table {table}"))
}

fn oracle_rows(oracle: &PathBuf, db: &PathBuf, sql: &str) -> String {
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

fn our_rows(db: &Path, header: &DatabaseHeader, schema: &TableSchema, sql: &str) -> String {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, schema).unwrap_or_else(|e| panic!("compiling: {e}"));
    let vfs = UnixVfs;
    let source: Rc<dyn PageSource> =
        Rc::new(VfsPageSource::open(&vfs, db, header.page_size).unwrap());
    let rows = execute_with_db(&program, source, *header).unwrap_or_else(|e| panic!("exec: {e}"));
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|v| match v {
                    sqlite_rs::record::Value::Null => String::new(),
                    sqlite_rs::record::Value::Integer(i) => i.to_string(),
                    sqlite_rs::record::Value::Real(r) => r.to_string(),
                    sqlite_rs::record::Value::Text(s) => s.to_string(),
                    sqlite_rs::record::Value::Blob(_) => "<blob>".to_string(),
                })
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compiled_opcodes(schema: &TableSchema, sql: &str) -> Vec<Opcode> {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, schema).unwrap_or_else(|e| panic!("compiling: {e}"));
    program.instructions.iter().map(|i| i.opcode).collect()
}

/// Confirms the covering-index-scan fast path was taken: a `SeekIndexEq`
/// probe against the index cursor (with `Column` reads straight off it —
/// real SQLite reuses the same `Column` opcode for index cursors, so its
/// presence alone doesn't distinguish the fast path) but no `SeekRowid`
/// — i.e. the table row is never fetched at all.
fn assert_covering_index_scan(schema: &TableSchema, sql: &str) {
    let opcodes = compiled_opcodes(schema, sql);
    assert!(
        opcodes.contains(&Opcode::SeekIndexEq),
        "expected SeekIndexEq in the compiled program for {sql:?}, got: {opcodes:?}"
    );
    assert!(
        !opcodes.contains(&Opcode::SeekRowid),
        "expected no SeekRowid (table lookup) for covering-index scan {sql:?}, got: {opcodes:?}"
    );
}

/// Confirms the index-only `COUNT(*)` fast path was taken: an
/// `IdxRewind`/`SeekIndexEq` probe, never a `Rewind`/`Next` table scan.
fn assert_index_only_count(schema: &TableSchema, sql: &str) {
    let opcodes = compiled_opcodes(schema, sql);
    let uses_index = opcodes.contains(&Opcode::IdxRewind) || opcodes.contains(&Opcode::SeekIndexEq);
    assert!(
        uses_index,
        "expected IdxRewind/SeekIndexEq in the compiled program for {sql:?}, got: {opcodes:?}"
    );
    assert!(
        !opcodes.contains(&Opcode::Rewind),
        "expected no table Rewind for index-only COUNT {sql:?}, got: {opcodes:?}"
    );
}

#[test]
fn covering_index_equality_select_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("no_stats_optimizations");
        return;
    };
    let db = scratch_db("covering");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, payload TEXT); \
         CREATE UNIQUE INDEX idx_ab ON t(a, b); \
         INSERT INTO t(a, b, payload) VALUES (5, 10, 'x'), (6, 11, 'y'), (7, 12, 'z');",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes.len(), 1);

    let sql = "SELECT a, b FROM t WHERE a = 6";
    assert_covering_index_scan(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn covering_index_equality_select_miss_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("no_stats_optimizations");
        return;
    };
    let db = scratch_db("covering-miss");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER); \
         CREATE UNIQUE INDEX idx_ab ON t(a, b); \
         INSERT INTO t(a, b) VALUES (5, 10), (6, 11);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT a, b FROM t WHERE a = 999";
    assert_covering_index_scan(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn index_only_count_star_no_where_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("no_stats_optimizations");
        return;
    };
    let db = scratch_db("count-all");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER); \
         CREATE INDEX idx_a ON t(a); \
         INSERT INTO t(a) VALUES (1), (2), (3), (4), (5);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT count(*) FROM t";
    assert_index_only_count(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn index_only_count_star_equality_where_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("no_stats_optimizations");
        return;
    };
    let db = scratch_db("count-eq");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER); \
         CREATE UNIQUE INDEX idx_a ON t(a); \
         INSERT INTO t(a) VALUES (1), (2), (3);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    for sql in [
        "SELECT count(*) FROM t WHERE a = 2",
        "SELECT count(*) FROM t WHERE a = 999",
    ] {
        assert_index_only_count(&schema, sql);
        assert_eq!(
            our_rows(&db, &header, &schema, sql),
            oracle_rows(&oracle, &db, sql),
            "mismatch for {sql:?}"
        );
    }
}
