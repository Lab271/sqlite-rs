// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #296 acceptance: `SELECT ... ORDER BY <indexed col> [DESC] LIMIT n`
//! compiles to an index-ordered scan (`IdxRewind`/`IdxNext` or
//! `IdxLast`/`IdxPrev` + `IdxRowid` + `SeekRowid`) instead of the
//! `Rewind`/`Next` + sorter pipeline, and produces byte-for-byte the
//! same rows as the pinned oracle. Seeds a table + index via the oracle
//! (so the index root page exists), reads its `TableSchema` (#211's
//! `indexes` catalog) via `read_schema`, then runs our own
//! `compile_select` + `execute_with_db` and diffs the output against
//! the oracle's own row output — the same scratch-db-plus-oracle
//! pattern `index_maintenance_test.rs` and `select_test.rs` already use.

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
        "sqlite-rs-index-ordered-scan-{label}-{}-{n}",
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

/// Confirms the fast path was actually taken (not merely correct by
/// accident via the sorter): the compiled program uses `IdxRewind`/
/// `IdxNext`/`IdxLast`/`IdxPrev`, never `SorterOpen`.
fn assert_index_ordered_scan(schema: &TableSchema, sql: &str) {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, schema).unwrap_or_else(|e| panic!("compiling: {e}"));
    let opcodes: Vec<Opcode> = program.instructions.iter().map(|i| i.opcode).collect();
    assert!(
        !opcodes.contains(&Opcode::SorterOpen),
        "expected an index-ordered scan (no sorter) for {sql:?}, got: {opcodes:?}"
    );
    let uses_index_scan =
        opcodes.contains(&Opcode::IdxRewind) || opcodes.contains(&Opcode::IdxLast);
    assert!(
        uses_index_scan,
        "expected IdxRewind/IdxLast in the compiled program for {sql:?}, got: {opcodes:?}"
    );
}

#[test]
fn ascending_order_by_matches_oracle_via_index_forward_walk() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_scan");
        return;
    };
    let db = scratch_db("asc");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER, payload TEXT); \
         CREATE INDEX idx_x ON t(x); \
         INSERT INTO t(x, payload) \
         SELECT (17 * value) % 97, 'row-' || value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes.len(), 1);

    let sql = "SELECT id, x, payload FROM t ORDER BY x ASC LIMIT 15";
    assert_index_ordered_scan(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn descending_order_by_matches_oracle_via_index_backward_walk() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_scan");
        return;
    };
    let db = scratch_db("desc");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER, payload TEXT); \
         CREATE INDEX idx_x ON t(x); \
         INSERT INTO t(x, payload) \
         SELECT (17 * value) % 97, 'row-' || value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes.len(), 1);

    // The index is ASC on x; ORDER BY x DESC needs the *reverse* walk
    // (`IdxLast`/`IdxPrev`) — this is the specific shape the issue
    // calls out ("a case where index order is reverse of ORDER BY").
    let sql = "SELECT id, x, payload FROM t ORDER BY x DESC LIMIT 15";
    assert_index_ordered_scan(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn descending_index_column_with_ascending_order_by_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_scan");
        return;
    };
    let db = scratch_db("desc_index");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER, payload TEXT); \
         CREATE INDEX idx_x_desc ON t(x DESC); \
         INSERT INTO t(x, payload) \
         SELECT (17 * value) % 97, 'row-' || value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes.len(), 1);

    // The index is declared DESC on x; ORDER BY x ASC needs the reverse
    // walk relative to the index's own declared direction.
    let sql = "SELECT id, x, payload FROM t ORDER BY x ASC LIMIT 15";
    assert_index_ordered_scan(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn order_by_limit_offset_matches_oracle_via_index_scan() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_scan");
        return;
    };
    let db = scratch_db("limit_offset");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER, payload TEXT); \
         CREATE INDEX idx_x ON t(x); \
         INSERT INTO t(x, payload) \
         SELECT (17 * value) % 97, 'row-' || value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT id, x, payload FROM t ORDER BY x DESC LIMIT 10 OFFSET 5";
    assert_index_ordered_scan(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

/// A `WHERE` clause present (even one the index could theoretically
/// also serve) falls back to the sorter path under this MVP's
/// conservative guardrail — confirms the fallback still yields correct
/// rows, not just that the fast path does.
#[test]
fn order_by_with_where_falls_back_to_sorter_and_still_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_scan");
        return;
    };
    let db = scratch_db("where_fallback");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER, payload TEXT); \
         CREATE INDEX idx_x ON t(x); \
         INSERT INTO t(x, payload) \
         SELECT value, 'row-' || value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT id, x, payload FROM t WHERE id > 5 ORDER BY x DESC LIMIT 10";
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, &schema).unwrap();
    let opcodes: Vec<Opcode> = program.instructions.iter().map(|i| i.opcode).collect();
    assert!(
        opcodes.contains(&Opcode::SorterOpen),
        "expected the sorter fallback for a WHERE-guarded ORDER BY, got: {opcodes:?}"
    );
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}
