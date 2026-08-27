// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #485 acceptance: `SELECT ... WHERE <non-leading indexed col> = ?`
//! against a composite index compiles to a skip-scan (`IdxRewind`/
//! `IdxNext` walking the whole index, `IdxRowid` + `SeekRowid` fetching
//! the full row only for a match) instead of a full `Rewind`/`Next`
//! table scan, whenever the leading column's `ANALYZE`-derived `avg_eq`
//! clears the oracle-confirmed skip-scan threshold (`src/planner.rs`'s
//! `SKIP_SCAN_MIN_AVG_EQ`) — and stays a full table scan (no `ANALYZE`,
//! or a high-cardinality leading column) exactly when oracle sqlite3
//! does too. Same scratch-db-plus-oracle pattern
//! `index_ordered_scan_test.rs` uses.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_select_with_catalog_and_stats;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::planner::{load_stats, Stats};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, Opcode};
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-skip-scan-{label}-{}-{n}",
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

fn all_schemas(db: &Path, header: &DatabaseHeader) -> Vec<TableSchema> {
    let vfs = UnixVfs;
    let source = VfsPageSource::open(&vfs, db, header.page_size).unwrap();
    let mut cursor = TableCursor::new(source, header, 1);
    read_schema(&mut cursor, header.text_encoding).unwrap()
}

fn table_stats(db: &Path, header: &DatabaseHeader, schemas: &[TableSchema], table: &str) -> Stats {
    let vfs = UnixVfs;
    let source = VfsPageSource::open(&vfs, db, header.page_size).unwrap();
    load_stats(source, header, schemas)
        .get(table)
        .cloned()
        .unwrap_or_default()
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

fn our_rows(
    db: &Path,
    header: &DatabaseHeader,
    schema: &TableSchema,
    stats: &Stats,
    sql: &str,
) -> String {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program =
        compile_select_with_catalog_and_stats(&select, schema, std::slice::from_ref(schema), stats)
            .unwrap_or_else(|e| panic!("compiling: {e}"));
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

fn compiled_opcodes(schema: &TableSchema, stats: &Stats, sql: &str) -> Vec<Opcode> {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    let program =
        compile_select_with_catalog_and_stats(&select, schema, std::slice::from_ref(schema), stats)
            .unwrap_or_else(|e| panic!("compiling: {e}"));
    program.instructions.iter().map(|i| i.opcode).collect()
}

/// A low-cardinality leading column (`category`, 3 distinct values over
/// 3000 rows — `avg_eq` well above the oracle-confirmed threshold of
/// ~18) makes a skip-scan over `idx(category, price)` worthwhile for
/// `WHERE price = ?`. Compiles to `IdxRewind`/`IdxNext` (not a
/// `Rewind`/`Next` table scan), and matches oracle row-for-row.
#[test]
fn low_cardinality_leading_column_compiles_to_skip_scan_matching_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("skip_scan");
        return;
    };
    let db = scratch_db("low-card");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, category TEXT, price INTEGER); \
         CREATE INDEX idx ON t(category, price); \
         INSERT INTO t(category, price) \
         SELECT 'cat' || (value % 3), value FROM generate_series(1, 3000); \
         ANALYZE;",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schemas = all_schemas(&db, &header);
    let schema = schemas
        .iter()
        .find(|s| s.name == "t")
        .expect("no schema for table t")
        .clone();
    assert_eq!(schema.indexes.len(), 1);
    let stats = table_stats(&db, &header, &schemas, "t");

    let sql = "SELECT id, price FROM t WHERE price = 1000";
    let opcodes = compiled_opcodes(&schema, &stats, sql);
    assert!(
        opcodes.contains(&Opcode::IdxRewind),
        "expected a skip-scan (IdxRewind) for {sql:?}, got: {opcodes:?}"
    );
    assert_eq!(
        our_rows(&db, &header, &schema, &stats, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

/// Without `ANALYZE` having ever run, skip-scan is never chosen — the
/// `WHERE price = ?` query on the same schema/data as the test above
/// falls back to a full table scan, matching oracle's own behavior of
/// never picking skip-scan absent `ANALYZE` history.
#[test]
fn no_analyze_falls_back_to_full_scan_matching_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("skip_scan");
        return;
    };
    let db = scratch_db("no-analyze");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, category TEXT, price INTEGER); \
         CREATE INDEX idx ON t(category, price); \
         INSERT INTO t(category, price) \
         SELECT 'cat' || (value % 3), value FROM generate_series(1, 3000);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schemas = all_schemas(&db, &header);
    let schema = schemas
        .iter()
        .find(|s| s.name == "t")
        .expect("no schema for table t")
        .clone();
    let stats = table_stats(&db, &header, &schemas, "t");
    assert_eq!(stats, Stats::default());

    let sql = "SELECT id, price FROM t WHERE price = 1000";
    let opcodes = compiled_opcodes(&schema, &stats, sql);
    assert!(
        !opcodes.contains(&Opcode::IdxRewind),
        "expected a plain table scan (no IdxRewind) without ANALYZE for {sql:?}, got: {opcodes:?}"
    );
    assert_eq!(
        our_rows(&db, &header, &schema, &stats, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

/// A high-cardinality leading column (`category`, ~3000 distinct values
/// over 3000 rows — `avg_eq = 1`, well below the ~18 threshold) never
/// picks skip-scan even with `ANALYZE` history, matching oracle's own
/// cost-based fallback to a full scan.
#[test]
fn high_cardinality_leading_column_falls_back_to_full_scan_matching_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("skip_scan");
        return;
    };
    let db = scratch_db("high-card");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, category TEXT, price INTEGER); \
         CREATE INDEX idx ON t(category, price); \
         INSERT INTO t(category, price) \
         SELECT 'cat' || value, value FROM generate_series(1, 3000); \
         ANALYZE;",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schemas = all_schemas(&db, &header);
    let schema = schemas
        .iter()
        .find(|s| s.name == "t")
        .expect("no schema for table t")
        .clone();
    let stats = table_stats(&db, &header, &schemas, "t");

    let sql = "SELECT id, price FROM t WHERE price = 1000";
    let opcodes = compiled_opcodes(&schema, &stats, sql);
    assert!(
        !opcodes.contains(&Opcode::IdxRewind),
        "expected a plain table scan (no IdxRewind) for a high-cardinality leading column, \
         got: {opcodes:?}"
    );
    assert_eq!(
        our_rows(&db, &header, &schema, &stats, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

/// #485 phase 3: `EXPLAIN QUERY PLAN` reports the oracle-confirmed
/// skip-scan text (`SEARCH t USING INDEX idx (ANY(category) AND
/// price=?)`) via the `sqlite-rs` CLI end to end — this exercises
/// `query.rs`'s real `stats_by_table` plumbing (loaded from
/// `sqlite_stat1` on disk), not the in-process `Stats` this file's
/// other tests build directly.
#[test]
fn explain_query_plan_reports_skip_scan_text_matching_oracle() {
    let db = scratch_db("eqp-skip-scan");
    let ddls = [
        "CREATE TABLE t(id INTEGER PRIMARY KEY, category TEXT, price INTEGER)",
        "CREATE INDEX idx ON t(category, price)",
    ];
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("skip_scan_eqp");
        return;
    };
    for stmt in ddls {
        let status = Command::new(&oracle).arg(&db).arg(stmt).status().unwrap();
        assert!(status.success(), "oracle setup failed: {stmt}");
    }
    let status = Command::new(&oracle)
        .arg(&db)
        .arg(
            "INSERT INTO t(category, price) \
             SELECT 'cat' || (value % 3), value FROM generate_series(1, 3000)",
        )
        .status()
        .unwrap();
    assert!(status.success());

    let status = Command::new(CLI)
        .arg("exec")
        .arg(&db)
        .arg("ANALYZE")
        .status()
        .unwrap();
    assert!(status.success(), "ANALYZE failed");

    let output = Command::new(CLI)
        .arg("query")
        .arg(&db)
        .arg("EXPLAIN QUERY PLAN SELECT * FROM t WHERE price = 1000")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "0|0|0|SEARCH t USING INDEX idx (ANY(category) AND price=?)",
    );
}
