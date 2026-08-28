// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #310 acceptance: an explicit `GROUP BY <indexed col(s)>` with no
//! `WHERE` clause compiles to a direct index-ordered walk (`IdxRewind`/
//! `IdxNext` + `IdxRowid` + `SeekRowid`) instead of
//! `compile_grouped_scan`'s `SorterOpen`/full-table-buffer/`SorterSort`
//! pipeline, and produces byte-for-byte the same rows as the pinned
//! oracle. Mirrors `index_ordered_scan_test.rs`'s scratch-db-plus-oracle
//! pattern for #296's `ORDER BY` fast path.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::codegen::compile_select;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, Opcode};
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-index-ordered-group-by-{label}-{}-{n}",
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
    let pager = sqlite_rs::pager::Pager::open(&vfs, db, page_size).unwrap();
    let raw = pager.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&raw[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

fn table_schema(db: &Path, header: &DatabaseHeader, table: &str) -> TableSchema {
    let vfs = UnixVfs;
    let source = VfsPageSource::open(&vfs, db, header.page_size).unwrap();
    let mut cursor = sqlite_rs::btree::TableCursor::new(source, header, 1);
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
fn assert_index_ordered_group_by(schema: &TableSchema, sql: &str) {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, schema).unwrap_or_else(|e| panic!("compiling: {e}"));
    let opcodes: Vec<Opcode> = program.instructions.iter().map(|i| i.opcode).collect();
    assert!(
        !opcodes.contains(&Opcode::SorterOpen),
        "expected an index-ordered GROUP BY (no sorter) for {sql:?}, got: {opcodes:?}"
    );
    assert!(
        opcodes.contains(&Opcode::IdxRewind),
        "expected IdxRewind in the compiled program for {sql:?}, got: {opcodes:?}"
    );
}

#[test]
fn single_column_group_by_matches_oracle_via_index_walk() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_group_by");
        return;
    };
    let db = scratch_db("single");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         CREATE INDEX idx_bucket ON t(bucket); \
         INSERT INTO t(bucket, x) \
         SELECT value % 11, value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes.len(), 1);

    let sql = "SELECT bucket, count(*), sum(x) FROM t GROUP BY bucket";
    assert_index_ordered_group_by(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn multi_column_group_by_matches_oracle_via_covering_index_walk() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_group_by");
        return;
    };
    let db = scratch_db("multi");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, x INTEGER); \
         CREATE INDEX idx_ab ON t(a, b); \
         INSERT INTO t(a, b, x) \
         SELECT value % 5, value % 7, value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes.len(), 1);

    let sql = "SELECT a, b, count(*), sum(x) FROM t GROUP BY a, b";
    assert_index_ordered_group_by(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn group_by_with_having_matches_oracle_via_index_walk() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_group_by");
        return;
    };
    let db = scratch_db("having");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         CREATE INDEX idx_bucket ON t(bucket); \
         INSERT INTO t(bucket, x) \
         SELECT value % 11, value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT bucket, count(*) FROM t GROUP BY bucket HAVING count(*) > 15";
    assert_index_ordered_group_by(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn group_by_over_zero_rows_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_group_by");
        return;
    };
    let db = scratch_db("empty");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         CREATE INDEX idx_bucket ON t(bucket);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT bucket, count(*) FROM t GROUP BY bucket";
    assert_eq!(our_rows(&db, &header, &schema, sql), "");
    assert_eq!(oracle_rows(&oracle, &db, sql), "");
}

/// A `WHERE` clause present makes the index fast path decline under
/// this MVP's conservative guardrail (matching #296's own) — confirms
/// the fallback (the sorter, #631) still yields correct rows, not just
/// that the fast path does.
#[test]
fn group_by_with_where_falls_back_to_the_sorter_and_still_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_group_by");
        return;
    };
    let db = scratch_db("where_fallback");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         CREATE INDEX idx_bucket ON t(bucket); \
         INSERT INTO t(bucket, x) \
         SELECT value % 11, value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT bucket, count(*) FROM t WHERE x > 5 GROUP BY bucket";
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, &schema).unwrap();
    let opcodes: Vec<Opcode> = program.instructions.iter().map(|i| i.opcode).collect();
    // #570/#631: the index fast path still declines a WHERE-guarded
    // GROUP BY (no cardinality estimate to judge an index walk against
    // a filtered table scan), and the fallback is the sorter — either
    // way, not the index walk this file is about.
    assert!(
        !opcodes.contains(&Opcode::IdxRewind),
        "expected the index fast path to decline a WHERE-guarded GROUP BY, got: {opcodes:?}"
    );
    assert!(
        opcodes.contains(&Opcode::SorterOpen),
        "expected the sorter fallback for a WHERE-guarded GROUP BY, got: {opcodes:?}"
    );
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

/// A `GROUP BY` over a computed expression (not a bare column) has no
/// corresponding index column to match against — falls back to the
/// sorter (#631), still correct.
#[test]
fn group_by_over_expression_falls_back_to_the_sorter_and_still_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_ordered_group_by");
        return;
    };
    let db = scratch_db("expr_fallback");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER); \
         CREATE INDEX idx_bucket ON t(bucket); \
         INSERT INTO t(bucket, x) \
         SELECT value % 11, value FROM generate_series(1, 200);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT bucket + 1, count(*) FROM t GROUP BY bucket + 1";
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, &schema).unwrap();
    let opcodes: Vec<Opcode> = program.instructions.iter().map(|i| i.opcode).collect();
    // #570/#631: as above — the index walk has no column to match a
    // computed GROUP BY expression against, and the sorter picks it up.
    assert!(
        !opcodes.contains(&Opcode::IdxRewind),
        "expected the index fast path to decline a computed GROUP BY, got: {opcodes:?}"
    );
    assert!(
        opcodes.contains(&Opcode::SorterOpen),
        "expected the sorter fallback for a computed GROUP BY expression, got: {opcodes:?}"
    );
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}
