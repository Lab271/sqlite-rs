// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #685 / spec 010 Req 8 acceptance: a write to a table carrying a
//! `sqlite_autoindex_*` must maintain that index or refuse the write —
//! never succeed and leave it stale.
//!
//! Every autoindex has `sql = NULL` in `sqlite_master`, so
//! `ddl_reader::index_schema` used to drop it, and the same
//! `TableSchema::indexes` list drives both uniqueness checking and index
//! maintenance in `codegen/stmt/insert.rs`. The result was a successful
//! write that left `PRAGMA integrity_check` reporting missing rows and
//! `count(*)` answering from the stale index.
//!
//! The numbering rule these tests pin down is oracle-derived, not
//! inferred — see #685 for the measurement table. Declaration order
//! wins, column-level constraints count, rowid-alias and `WITHOUT ROWID`
//! primary keys get nothing, and redundant constraints collapse.

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
        "sqlite-rs-autoindex-{label}-{}-{n}",
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

/// Runs one `INSERT` through our own parse -> codegen -> VDBE path.
fn our_insert(db: &Path, table: &str, sql: &str) -> Result<(), String> {
    let page_size = page_size_of(db);
    let header = read_header(db, page_size);
    let schema = table_schema(db, &header, table);
    let insert = match parse_insert(sql) {
        ParseOutcome::Accepted(i) => *i,
        other => panic!("failed to parse {sql}: {other:?}"),
    };
    let program = compile_insert(&insert, &schema, None).map_err(|e| format!("{e:?}"))?;
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header)
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// The name -> key-columns mapping our reader recovered, for asserting
/// the numbering rule.
fn autoindex_map(db: &Path, table: &str) -> Vec<(String, Vec<String>)> {
    let page_size = page_size_of(db);
    let header = read_header(db, page_size);
    let schema = table_schema(db, &header, table);
    let mut out: Vec<(String, Vec<String>)> = schema
        .indexes
        .iter()
        .map(|i| {
            (
                i.name.clone(),
                i.columns.iter().map(|c| c.name.clone()).collect(),
            )
        })
        .collect();
    out.sort();
    out
}

/// Spec 010 Req 8: "A write to a stock-created composite-PK table keeps
/// the index consistent."
#[test]
fn stock_composite_pk_stays_consistent() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("autoindex_maintenance");
        return;
    };
    let db = scratch_db("consistent");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t (a TEXT NOT NULL, b TEXT NOT NULL, c TEXT NOT NULL, v TEXT, \
           PRIMARY KEY (a, b, c)); \
         INSERT INTO t VALUES ('c','ns','t1','v1');",
    );

    // The reader must now see the autoindex at all — this is the
    // regression the whole ticket is about.
    let map = autoindex_map(&db, "t");
    assert_eq!(
        map,
        vec![(
            "sqlite_autoindex_t_1".to_string(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        )],
        "the composite PRIMARY KEY's autoindex was not recovered"
    );

    our_insert(&db, "t", "INSERT INTO t VALUES ('c','ns','t2','v2')").expect("insert should land");

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "SELECT count(*) FROM t"), "2");
}

/// Spec 010 Req 8: "A duplicate against an autoindex-backed constraint
/// is refused."
#[test]
fn autoindex_duplicate_is_refused() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("autoindex_maintenance");
        return;
    };
    let db = scratch_db("duplicate");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t (a TEXT NOT NULL, b TEXT NOT NULL, c TEXT NOT NULL, v TEXT, \
           PRIMARY KEY (a, b, c)); \
         INSERT INTO t VALUES ('c','ns','t1','v1');",
    );

    let err = our_insert(&db, "t", "INSERT INTO t VALUES ('c','ns','t1','dup')")
        .expect_err("a duplicate composite key must be refused");
    assert!(
        err.contains("UNIQUE"),
        "expected a UNIQUE constraint error, got {err}"
    );

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "SELECT count(*) FROM t"), "1");
}

/// Spec 010 Req 8: "A named index is unaffected." The control that
/// proves the fix did not achieve consistency by disabling writes.
#[test]
fn named_index_round_trips() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("autoindex_maintenance");
        return;
    };
    let db = scratch_db("named");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t (x TEXT, y TEXT); \
         CREATE UNIQUE INDEX u_xy ON t(x, y); \
         INSERT INTO t VALUES ('a','b');",
    );

    our_insert(&db, "t", "INSERT INTO t VALUES ('a','c')").expect("new key should land");
    let err = our_insert(&db, "t", "INSERT INTO t VALUES ('a','b')")
        .expect_err("duplicate should be refused");
    assert!(err.contains("UNIQUE"), "got {err}");

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(oracle_select(&oracle, &db, "SELECT count(*) FROM t"), "2");
}

/// A rowid alias gets no autoindex, so the reader must not invent one —
/// a phantom entry would make codegen open a cursor on a root page that
/// holds table data.
#[test]
fn rowid_alias_gains_no_phantom_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("autoindex_maintenance");
        return;
    };
    let db = scratch_db("rowid-alias");
    seed(
        &oracle,
        &db,
        "CREATE TABLE inline (a INTEGER PRIMARY KEY, b TEXT); \
         CREATE TABLE tabled (a INTEGER, b TEXT, PRIMARY KEY (a)); \
         CREATE TABLE composite (a INTEGER, b TEXT, PRIMARY KEY (a, b));",
    );

    assert!(
        autoindex_map(&db, "inline").is_empty(),
        "INTEGER PRIMARY KEY is the rowid; no index exists"
    );
    assert!(
        autoindex_map(&db, "tabled").is_empty(),
        "table-level PRIMARY KEY(a) on an INTEGER column is also the rowid"
    );
    // A composite key over an INTEGER column is *not* a rowid alias.
    assert_eq!(
        autoindex_map(&db, "composite"),
        vec![(
            "sqlite_autoindex_composite_1".to_string(),
            vec!["a".to_string(), "b".to_string()]
        )]
    );
}

/// `WITHOUT ROWID` stores rows in the primary-key b-tree itself, so the
/// primary key gets no separate index — but a `UNIQUE` still does.
#[test]
fn without_rowid_primary_key_gains_no_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("autoindex_maintenance");
        return;
    };
    let db = scratch_db("without-rowid");
    seed(
        &oracle,
        &db,
        "CREATE TABLE w (a TEXT, b TEXT, c TEXT, UNIQUE (c), PRIMARY KEY (a, b)) WITHOUT ROWID;",
    );
    assert_eq!(
        autoindex_map(&db, "w"),
        vec![("sqlite_autoindex_w_1".to_string(), vec!["c".to_string()])],
        "only the UNIQUE gets an index under WITHOUT ROWID"
    );
}

/// The numbering rule, which is the part most easily got wrong:
/// declaration order decides, not primary-key-first.
#[test]
fn autoindex_numbering_follows_declaration_order() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("autoindex_maintenance");
        return;
    };
    let db = scratch_db("numbering");
    seed(
        &oracle,
        &db,
        // UNIQUE declared *before* the PRIMARY KEY.
        "CREATE TABLE ord (a TEXT, b TEXT, c TEXT, UNIQUE (c), PRIMARY KEY (a, b)); \
         CREATE TABLE lvl (a TEXT PRIMARY KEY, b TEXT UNIQUE); \
         CREATE TABLE skip (a INTEGER PRIMARY KEY, b TEXT UNIQUE); \
         CREATE TABLE dup (a TEXT, b TEXT, PRIMARY KEY (a), UNIQUE (a));",
    );

    assert_eq!(
        autoindex_map(&db, "ord"),
        vec![
            ("sqlite_autoindex_ord_1".to_string(), vec!["c".to_string()]),
            (
                "sqlite_autoindex_ord_2".to_string(),
                vec!["a".to_string(), "b".to_string()]
            ),
        ],
        "declaration order must win over primary-key-first"
    );
    assert_eq!(
        autoindex_map(&db, "lvl"),
        vec![
            ("sqlite_autoindex_lvl_1".to_string(), vec!["a".to_string()]),
            ("sqlite_autoindex_lvl_2".to_string(), vec!["b".to_string()]),
        ],
        "column-level constraints get autoindexes too"
    );
    assert_eq!(
        autoindex_map(&db, "skip"),
        vec![("sqlite_autoindex_skip_1".to_string(), vec!["b".to_string()])],
        "a rowid-alias PK consumes no number, so UNIQUE(b) is _1"
    );
    assert_eq!(
        autoindex_map(&db, "dup"),
        vec![("sqlite_autoindex_dup_1".to_string(), vec!["a".to_string()])],
        "PRIMARY KEY(a) and UNIQUE(a) collapse to one index"
    );
}

/// The error message must name the columns, as stock SQLite does, not
/// the generated index name — which would be meaningless to a caller.
#[test]
fn unique_violation_message_names_columns_like_the_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("autoindex_maintenance");
        return;
    };
    let db = scratch_db("message");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t (a TEXT, b TEXT, c TEXT, PRIMARY KEY (a, b, c)); \
         INSERT INTO t VALUES ('x','y','z');",
    );
    let err = our_insert(&db, "t", "INSERT INTO t VALUES ('x','y','z')").expect_err("duplicate");
    assert!(
        err.contains("UNIQUE constraint failed: t.a, t.b, t.c"),
        "message should match the oracle's column list, got {err}"
    );
}
