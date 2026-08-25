//! #500 acceptance: a column/index-column declared `COLLATE NOCASE` (or
//! `RTRIM`) is consulted by comparisons that don't spell out an explicit
//! `COLLATE` in the query text — `SeekIndexEq`, the #450/#492
//! duplicate-key recheck, `ORDER BY`, and `GROUP BY` — matching real
//! `sqlite3` byte-for-byte. Same scratch-db-plus-oracle pattern
//! `no_stats_optimizations_test.rs` uses.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_select;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Collation;
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, Opcode};
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-declared-collate-{label}-{}-{n}",
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

/// A `COLLATE NOCASE` column declaration is captured on the table
/// schema, defaulting every other column to `Binary`.
#[test]
fn table_schema_captures_declared_column_collation() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("declared_collate");
        return;
    };
    let db = scratch_db("table-schema");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT COLLATE NOCASE, tag TEXT);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    assert_eq!(
        schema.column_collations,
        vec![Collation::Binary, Collation::NoCase, Collation::Binary]
    );
}

/// A `COLLATE NOCASE` index-column declaration is captured on the
/// index schema.
#[test]
fn index_schema_captures_declared_column_collation() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("declared_collate");
        return;
    };
    let db = scratch_db("index-schema");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
         CREATE INDEX idx_name ON t(name COLLATE NOCASE);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    assert_eq!(schema.indexes.len(), 1);
    assert_eq!(schema.indexes[0].columns[0].collation, Collation::NoCase);
}

/// `WHERE name = 'x'` against a `COLLATE NOCASE`-declared column, with
/// no `COLLATE` written in the query itself, matches case-varying rows —
/// exercising `SeekIndexEq`'s probe comparison via a covering-index scan.
#[test]
fn covering_index_seek_uses_declared_collation_without_explicit_collate() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("declared_collate");
        return;
    };
    let db = scratch_db("seek");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT COLLATE NOCASE); \
         CREATE INDEX idx_name ON t(name COLLATE NOCASE); \
         INSERT INTO t(name) VALUES ('Alice'), ('alice'), ('ALICE'), ('bob');",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT name FROM t WHERE name = 'alice'";
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

/// Same as above, but with several `NOCASE`-equal duplicate rows plus a
/// distinct trailing row — every case-varying duplicate must still
/// match (or, for `count(*)`, be counted), never just the first.
#[test]
fn declared_collation_matches_every_case_varying_duplicate() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("declared_collate");
        return;
    };
    let db = scratch_db("duplicates");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT COLLATE NOCASE); \
         CREATE INDEX idx_name ON t(name COLLATE NOCASE); \
         INSERT INTO t(name) VALUES ('Alice'), ('alice'), ('ALICE'), ('bob'), ('Bob');",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    for sql in [
        "SELECT name FROM t WHERE name = 'alice'",
        "SELECT name FROM t WHERE name = 'bob'",
        "SELECT count(*) FROM t WHERE name = 'alice'",
    ] {
        assert_eq!(
            our_rows(&db, &header, &schema, sql),
            oracle_rows(&oracle, &db, sql),
            "mismatch for {sql:?}"
        );
    }
}

/// Directly exercises `SeekIndexEq`'s probe comparison and the
/// #450/#492 duplicate-key recheck under a declared collation: an
/// `INTEGER`-keyed leading index column (satisfying the covering-index
/// fast path's integer-operand requirement) declared `COLLATE NOCASE`
/// still behaves identically to `BINARY` for integers, but proves the
/// P4 payload threading compiles and runs correctly end-to-end rather
/// than silently falling back to a full scan.
#[test]
fn covering_index_seek_and_recheck_compile_with_declared_collation_p4() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("declared_collate");
        return;
    };
    let db = scratch_db("seek-p4");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, bucket INTEGER COLLATE NOCASE, x INTEGER); \
         CREATE INDEX idx_bucket ON t(bucket COLLATE NOCASE); \
         INSERT INTO t(bucket, x) VALUES (1, 10), (2, 20), (2, 21), (2, 22), (3, 30);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes[0].columns[0].collation, Collation::NoCase);

    let sql = "SELECT bucket FROM t WHERE bucket = 2";
    let opcodes: Vec<Opcode> = {
        let select = match parse_select(sql) {
            ParseOutcome::Accepted(s) => *s,
            other => panic!("expected {sql:?} to parse, got {other:?}"),
        };
        let program = compile_select(&select, &schema).unwrap_or_else(|e| panic!("compiling: {e}"));
        program.instructions.iter().map(|i| i.opcode).collect()
    };
    assert!(
        opcodes.contains(&Opcode::SeekIndexEq),
        "expected SeekIndexEq for {sql:?}, got: {opcodes:?}"
    );
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

/// `ORDER BY` on a `COLLATE NOCASE`-declared column sorts
/// case-insensitively without an explicit `COLLATE` in the query.
#[test]
fn order_by_uses_declared_collation_without_explicit_collate() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("declared_collate");
        return;
    };
    let db = scratch_db("order-by");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT COLLATE NOCASE); \
         INSERT INTO t(name) VALUES ('bob'), ('Alice'), ('carol'), ('alice');",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT name FROM t ORDER BY name";
    assert_eq!(
        our_rows(&db, &header, &schema, sql),
        oracle_rows(&oracle, &db, sql)
    );
}

/// `GROUP BY` on a `COLLATE NOCASE`-declared column groups
/// case-insensitively without an explicit `COLLATE` in the query.
#[test]
fn group_by_uses_declared_collation_without_explicit_collate() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("declared_collate");
        return;
    };
    let db = scratch_db("group-by");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT COLLATE NOCASE); \
         INSERT INTO t(name) VALUES ('bob'), ('Bob'), ('BOB'), ('carol');",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT name, count(*) FROM t GROUP BY name";
    let sort = |rows: String| {
        let mut lines: Vec<String> = rows.lines().map(str::to_string).collect();
        lines.sort();
        lines.join("\n")
    };
    assert_eq!(
        sort(our_rows(&db, &header, &schema, sql)),
        sort(oracle_rows(&oracle, &db, sql))
    );
}
