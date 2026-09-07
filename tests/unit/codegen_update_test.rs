// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! #210 acceptance: `compile_update` against a real on-disk table,
//! covering a `WHERE`-filtered multi-row update (same-leaf-page
//! delete+reinsert mid-scan), an update that leaves some columns
//! untouched, and a rowid-alias reassignment — executed via
//! `execute_with_writable_db` and read back with `TableCursor`/
//! `decode_record`, mirroring `tests/unit/codegen_insert_test.rs`'s harness.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_insert, compile_update};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::{parse_insert, parse_update, ParseOutcome};
use sqlite_rs::record::{decode_record, TextEncoding, Value};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_writable_db, ExecError};
use sqlite_rs::vfs::{UnixVfs, Vfs, VfsPageSource};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-codegen-update-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

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
) {
    let insert = match parse_insert(sql) {
        ParseOutcome::Accepted(i) => *i,
        other => panic!("failed to parse {sql:?}: {other:?}"),
    };
    let program = compile_insert(&insert, schema, None).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, path, page_size).unwrap();
    execute_with_writable_db(&program, pager, *header).unwrap();
}

fn run_update(
    path: &Path,
    header: &DatabaseHeader,
    page_size: u32,
    sql: &str,
    schema: &TableSchema,
) -> Result<(), ExecError> {
    let update = match parse_update(sql) {
        ParseOutcome::Accepted(u) => *u,
        other => panic!("failed to parse {sql:?}: {other:?}"),
    };
    let program = compile_update(&update, schema).unwrap();
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
    let mut row = cursor.first_row().unwrap();
    while let Some(r) = row {
        let values = decode_record(&r.payload, TextEncoding::Utf8).unwrap();
        out.push((r.rowid, values));
        row = cursor.next_row().unwrap();
    }
    out
}

fn schema(sql: &str) -> TableSchema {
    TableSchema {
        unresolved_autoindex: false,
        name: "t".to_string(),
        root_page: 1,
        columns: vec!["a".to_string(), "b".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        column_collations: vec![],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
        rowid_alias: None,
    }
    .with_computed_rowid_alias()
}

fn schema_with_columns(sql: &str, columns: &[&str], column_types: &[&str]) -> TableSchema {
    TableSchema {
        unresolved_autoindex: false,
        name: "t".to_string(),
        root_page: 1,
        columns: columns.iter().map(|s| (*s).to_string()).collect(),
        column_types: column_types.iter().map(|s| (*s).to_string()).collect(),
        column_collations: vec![],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
        rowid_alias: None,
    }
    .with_computed_rowid_alias()
}

#[test]
fn where_filtered_update_touches_only_matching_rows_mid_scan() {
    let path = scratch_db("where-filtered");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let schema = schema("CREATE TABLE t(a INTEGER, b TEXT)");

    for i in 1..=5 {
        run_insert(
            &path,
            &header,
            page_size,
            &format!("INSERT INTO t(a, b) VALUES ({i}, 'v{i}')"),
            &schema,
        );
    }

    run_update(
        &path,
        &header,
        page_size,
        "UPDATE t SET b = 'x' WHERE a = 2 OR a = 4",
        &schema,
    )
    .unwrap();

    let got = rows(&path, &header, page_size, 1);
    let texts: Vec<(i64, String)> = got
        .into_iter()
        .map(|(rowid, values)| match &values[1] {
            Value::Text(s) => (rowid, s.to_string()),
            other => panic!("expected TEXT, got {other:?}"),
        })
        .collect();
    assert_eq!(
        texts,
        vec![
            (1, "v1".to_string()),
            (2, "x".to_string()),
            (3, "v3".to_string()),
            (4, "x".to_string()),
            (5, "v5".to_string()),
        ]
    );
}

#[test]
fn unassigned_columns_are_preserved() {
    let path = scratch_db("unassigned");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let schema = schema("CREATE TABLE t(a INTEGER, b TEXT)");

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(a, b) VALUES (1, 'keep')",
        &schema,
    );

    run_update(&path, &header, page_size, "UPDATE t SET a = 42", &schema).unwrap();

    let got = rows(&path, &header, page_size, 1);
    assert_eq!(
        got,
        vec![(
            1,
            vec![Value::Integer(42), Value::Text("keep".to_string().into())]
        )]
    );
}

#[test]
fn rowid_alias_reassignment_changes_the_stored_rowid() {
    let path = scratch_db("rowid-alias");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let schema = schema_with_columns(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT)",
        &["id", "b"],
        &["INTEGER", "TEXT"],
    );

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(id, b) VALUES (1, 'x')",
        &schema,
    );

    run_update(
        &path,
        &header,
        page_size,
        "UPDATE t SET id = 100 WHERE id = 1",
        &schema,
    )
    .unwrap();

    let got = rows(&path, &header, page_size, 1);
    assert_eq!(
        got,
        vec![(100, vec![Value::Null, Value::Text("x".to_string().into())])]
    );
}

#[test]
fn not_null_violation_halts_and_leaves_the_row_unchanged() {
    let path = scratch_db("update-notnull");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let schema = schema("CREATE TABLE t(a INTEGER NOT NULL, b TEXT)");

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(a, b) VALUES (1, 'x')",
        &schema,
    );

    let err = run_update(&path, &header, page_size, "UPDATE t SET a = NULL", &schema)
        .expect_err("NULL into a NOT NULL column must fail");
    match err {
        ExecError::Halted { code, .. } => assert_eq!(code, 1299),
        other => panic!("expected Halted, got {other:?}"),
    }

    assert_eq!(
        rows(&path, &header, page_size, 1),
        vec![(
            1,
            vec![Value::Integer(1), Value::Text("x".to_string().into())]
        )]
    );
}

/// #336 regression: `WHERE rowid = <int literal>` compiles to
/// `SeekRowid`, not a `Rewind`/`Next` scan.
#[test]
fn rowid_equality_update_compiles_to_seek_rowid() {
    let schema = schema("CREATE TABLE t(a INTEGER, b TEXT)");
    let update = match parse_update("UPDATE t SET b = 'x' WHERE rowid = 2") {
        ParseOutcome::Accepted(u) => *u,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_update(&update, &schema).unwrap();
    let rows = sqlite_rs::vdbe::explain(&program);
    assert!(
        rows.iter().any(|r| r.opcode == "SeekRowid"),
        "expected SeekRowid in the compiled program: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.opcode == "Rewind"),
        "rowid-equality update must not also emit a full scan: {rows:?}"
    );
}

/// #336 regression: a compound `WHERE` must NOT take the seek fast
/// path — only a single top-level equality does, matching #137's own
/// narrow scope for `SELECT`. Uses `a` as the rowid alias so the
/// fallback scan's ordinary column resolution can compile `WHERE a =
/// ...` (resolving the bare `rowid` keyword outside the seek fast paths
/// is a separate, pre-existing gap this test isn't about).
#[test]
fn compound_where_with_rowid_alias_equality_falls_back_to_scan() {
    let schema = schema_with_columns(
        "CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT)",
        &["a", "b"],
        &["INTEGER", "TEXT"],
    );
    let update = match parse_update("UPDATE t SET b = 'x' WHERE a = 2 OR a = 5") {
        ParseOutcome::Accepted(u) => *u,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_update(&update, &schema).unwrap();
    let rows = sqlite_rs::vdbe::explain(&program);
    assert!(
        rows.iter().any(|r| r.opcode == "Rewind"),
        "expected the compound WHERE to still fall back to a full scan: {rows:?}"
    );
}

#[test]
fn rowid_equality_update_touches_only_the_matching_row() {
    let path = scratch_db("rowid-seek");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let schema = schema("CREATE TABLE t(a INTEGER, b TEXT)");

    for i in 1..=5 {
        run_insert(
            &path,
            &header,
            page_size,
            &format!("INSERT INTO t(a, b) VALUES ({i}, 'v{i}')"),
            &schema,
        );
    }

    run_update(
        &path,
        &header,
        page_size,
        "UPDATE t SET b = 'x' WHERE rowid = 3",
        &schema,
    )
    .unwrap();

    let got = rows(&path, &header, page_size, 1);
    let texts: Vec<(i64, String)> = got
        .into_iter()
        .map(|(rowid, values)| match &values[1] {
            Value::Text(s) => (rowid, s.to_string()),
            other => panic!("expected TEXT, got {other:?}"),
        })
        .collect();
    assert_eq!(
        texts,
        vec![
            (1, "v1".to_string()),
            (2, "v2".to_string()),
            (3, "x".to_string()),
            (4, "v4".to_string()),
            (5, "v5".to_string()),
        ]
    );
}

/// A rowid that doesn't exist: the seek misses cleanly, no row updated.
#[test]
fn rowid_equality_update_with_no_such_rowid_is_a_no_op() {
    let path = scratch_db("rowid-seek-miss");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let schema = schema("CREATE TABLE t(a INTEGER, b TEXT)");

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(a, b) VALUES (1, 'x')",
        &schema,
    );

    run_update(
        &path,
        &header,
        page_size,
        "UPDATE t SET b = 'z' WHERE rowid = 999",
        &schema,
    )
    .unwrap();

    assert_eq!(
        rows(&path, &header, page_size, 1),
        vec![(
            1,
            vec![Value::Integer(1), Value::Text("x".to_string().into())]
        )]
    );
}

#[test]
fn check_violation_halts_and_leaves_the_row_unchanged() {
    let path = scratch_db("update-check");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let schema = schema_with_columns(
        "CREATE TABLE t(a INTEGER CHECK (a > 0))",
        &["a"],
        &["INTEGER"],
    );

    run_insert(
        &path,
        &header,
        page_size,
        "INSERT INTO t(a) VALUES (5)",
        &schema,
    );

    let err = run_update(&path, &header, page_size, "UPDATE t SET a = -1", &schema)
        .expect_err("a negative value must fail the CHECK constraint");
    match err {
        ExecError::Halted { code, .. } => assert_eq!(code, 275),
        other => panic!("expected Halted, got {other:?}"),
    }

    assert_eq!(
        rows(&path, &header, page_size, 1),
        vec![(1, vec![Value::Integer(5)])]
    );
}

// ---------------------------------------------------------------
// #666: index-seek range scan (`WHERE col >/>=/</<= lit`, `BETWEEN`)
// ---------------------------------------------------------------

fn seed_via_sqlite3(db: &Path, sql: &str) {
    let status = Command::new("sqlite3").arg(db).arg(sql).status().unwrap();
    assert!(status.success());
}

fn indexed_table_schema(db: &Path, table: &str) -> TableSchema {
    let vfs = UnixVfs;
    let file = vfs.open_read(db).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source = VfsPageSource::open(&vfs, db, header.page_size).unwrap();
    let mut cursor = TableCursor::new(source, &header, 1);
    let schemas = read_schema(&mut cursor, header.text_encoding).unwrap();
    schemas
        .into_iter()
        .find(|s| s.name == table)
        .unwrap_or_else(|| panic!("no schema for table {table}"))
}

fn range_seek_fixture(label: &str) -> (PathBuf, DatabaseHeader, u32, TableSchema) {
    let db = std::env::temp_dir().join(format!(
        "sqlite-rs-codegen-update-range-seek-{label}-{}.db",
        std::process::id()
    ));
    std::fs::remove_file(&db).ok();
    seed_via_sqlite3(
        &db,
        "CREATE TABLE t(id INTEGER, val INTEGER); \
         CREATE INDEX idx_val ON t(val); \
         INSERT INTO t VALUES (1, 5), (2, 10), (3, 15), (4, 20), (5, 25);",
    );
    let schema = indexed_table_schema(&db, "t");
    let vfs = UnixVfs;
    let file = vfs.open_read(&db).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let page_size = header.page_size;
    (db, header, page_size, schema)
}

/// #666: `WHERE val > lit` against a leading-indexed column compiles to
/// an index seek (`SeekIndexGE`), not a full `Rewind`/`Next` scan.
#[test]
fn range_predicate_update_compiles_to_index_seek() {
    let (_db, _header, _page_size, schema) = range_seek_fixture("compile-check");
    let update = match parse_update("UPDATE t SET id = id + 1 WHERE val > 15") {
        ParseOutcome::Accepted(u) => *u,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_update(&update, &schema).unwrap();
    let rows = sqlite_rs::vdbe::explain(&program);
    assert!(
        rows.iter().any(|r| r.opcode == "SeekIndexGE"),
        "expected SeekIndexGE in the compiled program: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.opcode == "Rewind" && r.p1 == 0),
        "range-predicate update must not also emit a full scan of the table cursor: {rows:?}"
    );
}

#[test]
fn range_predicate_update_touches_only_matching_rows() {
    let (db, header, page_size, schema) = range_seek_fixture("exec-gt");
    run_update(
        &db,
        &header,
        page_size,
        "UPDATE t SET id = id + 100 WHERE val > 15",
        &schema,
    )
    .unwrap();

    let got = rows(&db, &header, page_size, schema.root_page);
    let mut ids: Vec<i64> = got
        .into_iter()
        .map(|(_, values)| match &values[0] {
            Value::Integer(n) => *n,
            other => panic!("expected INTEGER, got {other:?}"),
        })
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2, 3, 104, 105]);
}

#[test]
fn between_predicate_update_touches_only_matching_rows() {
    let (db, header, page_size, schema) = range_seek_fixture("exec-between");
    run_update(
        &db,
        &header,
        page_size,
        "UPDATE t SET id = id + 100 WHERE val BETWEEN 10 AND 20",
        &schema,
    )
    .unwrap();

    let got = rows(&db, &header, page_size, schema.root_page);
    let mut ids: Vec<i64> = got
        .into_iter()
        .map(|(_, values)| match &values[0] {
            Value::Integer(n) => *n,
            other => panic!("expected INTEGER, got {other:?}"),
        })
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 5, 102, 103, 104]);
}

// ---------------------------------------------------------------
// #675: single-pass range-seek update when the `SET` clause doesn't
// touch the scanned index, still falling back to #666's two-pass plan
// when it does.
// ---------------------------------------------------------------

/// `SET id = ...` never touches `idx_val` (the index `WHERE val > lit`
/// scans), so the compiled plan must skip the ephemeral-rowid
/// materialization entirely and apply the update directly in the index
/// walk.
#[test]
fn range_predicate_update_without_indexed_set_uses_single_pass() {
    let (_db, _header, _page_size, schema) = range_seek_fixture("single-pass-compile");
    let update = match parse_update("UPDATE t SET id = id + 1 WHERE val > 15") {
        ParseOutcome::Accepted(u) => *u,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_update(&update, &schema).unwrap();
    let rows = sqlite_rs::vdbe::explain(&program);
    assert!(
        rows.iter().any(|r| r.opcode == "SeekIndexGE"),
        "expected SeekIndexGE in the compiled program: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.opcode == "OpenEphemeral"),
        "SET column doesn't intersect the scanned index — the two-pass \
         ephemeral-rowid plan should be skipped: {rows:?}"
    );
}

/// `SET val = ...` *does* touch `idx_val`, the very index `WHERE val >
/// lit` scans — the compiled plan must keep #666's two-pass ephemeral
/// plan (correctness over speed here), and the update must still land
/// correctly on every originally-matching row despite the self-mutating
/// index walk.
#[test]
fn range_predicate_update_on_indexed_column_keeps_two_pass_plan() {
    let (db, header, page_size, schema) = range_seek_fixture("two-pass-compile");
    let update = match parse_update("UPDATE t SET val = val + 1 WHERE val > 15") {
        ParseOutcome::Accepted(u) => *u,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_update(&update, &schema).unwrap();
    let rows_eqp = sqlite_rs::vdbe::explain(&program);
    assert!(
        rows_eqp.iter().any(|r| r.opcode == "OpenEphemeral"),
        "SET column intersects the scanned index — the two-pass \
         ephemeral-rowid plan must still be used: {rows_eqp:?}"
    );

    run_update(
        &db,
        &header,
        page_size,
        "UPDATE t SET val = val + 1 WHERE val > 15",
        &schema,
    )
    .unwrap();

    let got = rows(&db, &header, page_size, schema.root_page);
    let mut vals: Vec<i64> = got
        .into_iter()
        .map(|(_, values)| match &values[1] {
            Value::Integer(n) => *n,
            other => panic!("expected INTEGER, got {other:?}"),
        })
        .collect();
    vals.sort_unstable();
    // Originally 5, 10, 15, 20, 25 -- only val > 15 (20, 25) should be
    // touched, becoming 21, 26. A self-mutating single-pass walk would
    // either skip, re-visit, or double-increment these once the index
    // b-tree is rewritten mid-scan.
    assert_eq!(vals, vec![5, 10, 15, 21, 26]);
}
