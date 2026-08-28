// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #605 acceptance: an `OR`-chain of equalities against the same column
//! (`x = 1 OR x = 2 OR x = 3`) converts to one seek per value instead
//! of falling back to a full `Rewind`/`Next` table scan — for both the
//! rowid-seek fast path (`SeekRowid`) and the covering-index-scan fast
//! path (`SeekIndexEq`) — and produces byte-for-byte the same rows as
//! the pinned oracle. Same scratch-db-plus-oracle pattern
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
        "sqlite-rs-or-to-in-{label}-{}-{n}",
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

/// `rowid = 1 OR rowid = 3 OR rowid = 5` converts to three `SeekRowid`
/// probes instead of a full table scan, and matches the oracle exactly.
#[test]
fn rowid_or_chain_seeks_and_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("or_to_in");
        return;
    };
    let db = scratch_db("rowid");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(a INTEGER, b INTEGER); \
         INSERT INTO t VALUES (1, 10), (2, 20), (3, 30), (4, 40), (5, 50);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT a, b FROM t WHERE rowid = 1 OR rowid = 3 OR rowid = 5;";
    let opcodes = compiled_opcodes(&schema, sql);
    assert!(
        opcodes
            .iter()
            .filter(|op| **op == Opcode::SeekRowid)
            .count()
            == 3,
        "expected three SeekRowid probes: {opcodes:?}"
    );
    assert!(
        !opcodes.contains(&Opcode::Rewind),
        "OR-to-IN rowid seek must not also emit a full scan: {opcodes:?}"
    );

    let ours = our_rows(&db, &header, &schema, sql);
    let oracle_out = oracle_rows(&oracle, &db, sql);
    assert_eq!(ours, oracle_out);
}

/// `a = 1 OR a = 3 OR a = 5` against an indexed, non-rowid column
/// converts to three `SeekIndexEq` probes via the covering-index-scan
/// fast path, and matches the oracle exactly.
#[test]
fn covering_index_or_chain_seeks_and_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("or_to_in");
        return;
    };
    let db = scratch_db("covering");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(a INTEGER, b INTEGER); \
         CREATE INDEX idx_a ON t(a); \
         INSERT INTO t VALUES (1, 10), (2, 20), (3, 30), (4, 40), (5, 50);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    // Bare-column projection over just `a` so `bare_result_column_names`
    // + `idx_a`'s own column recognizes this as index-only (#444).
    let sql = "SELECT a FROM t WHERE a = 1 OR a = 3 OR a = 5;";
    let opcodes = compiled_opcodes(&schema, sql);
    assert!(
        opcodes
            .iter()
            .filter(|op| **op == Opcode::SeekIndexEq)
            .count()
            == 3,
        "expected three SeekIndexEq probes: {opcodes:?}"
    );
    assert!(
        !opcodes.contains(&Opcode::Rewind),
        "OR-to-IN covering-index scan must not also emit a full scan: {opcodes:?}"
    );

    let ours = our_rows(&db, &header, &schema, sql);
    let oracle_out = oracle_rows(&oracle, &db, sql);
    assert_eq!(ours, oracle_out);
}

/// An `OR`-chain mixing columns (`a = 1 OR b = 2`) can't convert — no
/// single fast path could enforce both branches — and must still return
/// the correct rows via the ordinary scan.
#[test]
fn mixed_column_or_falls_back_to_ordinary_scan_and_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("or_to_in");
        return;
    };
    let db = scratch_db("mixed");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(a INTEGER, b INTEGER); \
         INSERT INTO t VALUES (1, 10), (2, 20), (3, 30);",
    );
    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let sql = "SELECT a, b FROM t WHERE a = 1 OR b = 20;";
    let ours = our_rows(&db, &header, &schema, sql);
    let oracle_out = oracle_rows(&oracle, &db, sql);
    assert_eq!(ours, oracle_out);
}
