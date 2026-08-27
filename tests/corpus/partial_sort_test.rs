// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #574 acceptance: `SELECT ... ORDER BY <prefix cols>, <suffix cols>`
//! where an index satisfies a strict *prefix* of the requested order
//! (but not all of it) compiles to a per-prefix-group sort (walk the
//! index directly, sort only each group's suffix) instead of
//! `compile_sorted_scan`'s single sort over the whole result set —
//! and produces byte-for-byte the same rows as the pinned oracle.
//! Mirrors `index_ordered_scan_test.rs`'s scratch-db-plus-oracle
//! pattern.

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
        "sqlite-rs-partial-sort-{label}-{}-{n}",
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

/// Confirms the partial-sort path was actually taken: a sorter runs
/// (unlike the full-match `index_ordered_scan` path), but so does an
/// index walk (`IdxRewind`/`IdxLast`) — the full-table-scan-then-sort
/// path (`compile_sorted_scan`) never emits `IdxRewind`/`IdxLast`.
fn assert_partial_sorted_index_scan(schema: &TableSchema, sql: &str) {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program = compile_select(&select, schema).unwrap_or_else(|e| panic!("compiling: {e}"));
    let opcodes: Vec<Opcode> = program.instructions.iter().map(|i| i.opcode).collect();
    assert!(
        opcodes.contains(&Opcode::SorterOpen),
        "expected the partial-sort path's per-group sorter for {sql:?}, got: {opcodes:?}"
    );
    let uses_index_scan =
        opcodes.contains(&Opcode::IdxRewind) || opcodes.contains(&Opcode::IdxLast);
    assert!(
        uses_index_scan,
        "expected an index walk (IdxRewind/IdxLast) for {sql:?}, got: {opcodes:?}"
    );
}

fn seed_grouped_table(oracle: &PathBuf, db: &PathBuf) {
    seed(
        oracle,
        db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, payload TEXT); \
         CREATE INDEX idx_a ON t(a); \
         INSERT INTO t(a, b, payload) \
         SELECT value % 5, (37 * value) % 101, 'row-' || value \
         FROM generate_series(1, 300);",
    );
}

#[test]
fn order_by_prefix_plus_suffix_matches_oracle_via_partial_sort() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("partial_sort");
        return;
    };
    let db = scratch_db("prefix_suffix");
    seed_grouped_table(&oracle, &db);
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes.len(), 1);

    let sql = "SELECT id, a, b, payload FROM t ORDER BY a ASC, b ASC";
    assert_partial_sorted_index_scan(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn order_by_prefix_desc_suffix_asc_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("partial_sort");
        return;
    };
    let db = scratch_db("prefix_desc");
    seed_grouped_table(&oracle, &db);
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    // The index is ASC on `a`; ORDER BY a DESC needs the reverse walk
    // (`IdxLast`/`IdxPrev`) for the prefix, while `b ASC` is still
    // sorted normally within each group.
    let sql = "SELECT id, a, b, payload FROM t ORDER BY a DESC, b ASC";
    assert_partial_sorted_index_scan(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn order_by_prefix_suffix_with_limit_offset_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("partial_sort");
        return;
    };
    let db = scratch_db("limit_offset");
    seed_grouped_table(&oracle, &db);
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    // LIMIT/OFFSET must apply globally, across group boundaries, not
    // reset per group.
    let sql = "SELECT id, a, b, payload FROM t ORDER BY a ASC, b DESC LIMIT 20 OFFSET 15";
    assert_partial_sorted_index_scan(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

#[test]
fn order_by_prefix_suffix_with_nulls_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("partial_sort");
        return;
    };
    let db = scratch_db("nulls");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER); \
         CREATE INDEX idx_a ON t(a); \
         INSERT INTO t(a, b) VALUES \
         (1, 5), (1, NULL), (1, 2), \
         (NULL, 3), (NULL, 1), \
         (2, NULL), (2, 4), (2, NULL);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT id, a, b FROM t ORDER BY a ASC, b ASC";
    assert_partial_sorted_index_scan(&schema, sql);
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

/// A `WHERE` clause present falls back to the sorter path under this
/// MVP's conservative guardrail (same as `index_ordered_scan_test.rs`'s
/// equivalent case) — confirms the fallback still yields correct rows.
#[test]
fn order_by_prefix_suffix_with_where_falls_back_and_still_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("partial_sort");
        return;
    };
    let db = scratch_db("where_fallback");
    seed_grouped_table(&oracle, &db);
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT id, a, b, payload FROM t WHERE b > 10 ORDER BY a ASC, b ASC";
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}
