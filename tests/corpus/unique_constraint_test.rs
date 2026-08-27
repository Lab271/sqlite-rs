// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #207 acceptance: `compile_insert` enforces UNIQUE constraints on
//! non-rowid columns via the new `Opcode::NoConflict` real-index
//! seek+branch primitive (`src/vdbe/cursor.rs::no_conflict`), honoring
//! `ON CONFLICT` the same way #195 does for the rowid-PK case. Mirrors
//! `index_maintenance_test.rs`'s oracle-diff harness: seed a table plus
//! a `UNIQUE` index via the oracle, run our own codegen, and check the
//! oracle's own `PRAGMA integrity_check` plus row counts.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_insert;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::{parse_insert, ParseOutcome};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::execute_with_writable_db;
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-unique-constraint-{label}-{}-{n}",
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

/// Default `ON CONFLICT ABORT`: a duplicate UNIQUE key halts the
/// statement (`SQLITE_CONSTRAINT_UNIQUE`) and the row set is unchanged
/// — mirrors #195's rowid-PK `emit_pk_conflict` test shape.
#[test]
fn insert_rejects_duplicate_unique_key_by_default() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("unique_constraint");
        return;
    };
    let db = scratch_db("abort");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, email TEXT); \
         CREATE UNIQUE INDEX idx_email ON t(email); \
         INSERT INTO t VALUES (1, 'a@example.com');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert!(schema.indexes.iter().any(|i| i.unique));

    let insert = match parse_insert("INSERT INTO t VALUES (2, 'a@example.com')") {
        ParseOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema, None).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    let err = execute_with_writable_db(&program, pager, header).unwrap_err();
    assert!(
        format!("{err:?}").contains("UNIQUE"),
        "expected a UNIQUE constraint error, got {err:?}"
    );

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "SELECT count(*) FROM t"), "1");
}

/// `ON CONFLICT IGNORE`: the conflicting row is silently skipped, the
/// rest of a multi-row `VALUES` list still lands.
#[test]
fn insert_or_ignore_skips_duplicate_unique_key() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("unique_constraint");
        return;
    };
    let db = scratch_db("ignore");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, email TEXT); \
         CREATE UNIQUE INDEX idx_email ON t(email); \
         INSERT INTO t VALUES (1, 'a@example.com');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let insert = match parse_insert(
        "INSERT OR IGNORE INTO t VALUES (2, 'a@example.com'), (3, 'b@example.com')",
    ) {
        ParseOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema, None).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "SELECT count(*) FROM t"), "2");
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t WHERE email = 'b@example.com'"
        ),
        "3"
    );
}

/// `ON CONFLICT REPLACE`: the pre-existing row (and its index entries)
/// is deleted before the new one is written, keeping the index
/// consistent (checked via `PRAGMA integrity_check`, same as
/// `index_maintenance_test.rs`).
#[test]
fn insert_or_replace_displaces_the_conflicting_row() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("unique_constraint");
        return;
    };
    let db = scratch_db("replace");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, email TEXT); \
         CREATE UNIQUE INDEX idx_email ON t(email); \
         INSERT INTO t VALUES (1, 'a@example.com'), (2, 'b@example.com');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let insert = match parse_insert("INSERT OR REPLACE INTO t VALUES (99, 'a@example.com')") {
        ParseOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema, None).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "SELECT count(*) FROM t"), "2");
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t WHERE email = 'a@example.com'"
        ),
        "99"
    );
    assert_eq!(
        oracle_select(&oracle, &db, "SELECT count(*) FROM t WHERE id = 1"),
        "0"
    );
}
