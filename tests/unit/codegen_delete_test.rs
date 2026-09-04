// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! #210 acceptance: `compile_delete` against a real on-disk table,
//! covering an unconditional `DELETE FROM t`, a `WHERE`-filtered
//! multi-row delete (including same-leaf-page deletes mid-scan), and a
//! `WHERE` that matches nothing — executed via `execute_with_writable_db`
//! and read back with `TableCursor`/`decode_record`, mirroring
//! `tests/unit/codegen_insert_test.rs`'s harness.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_delete, compile_insert};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::{parse_delete, parse_insert, ParseOutcome};
use sqlite_rs::record::{decode_record, TextEncoding, Value};
use sqlite_rs::schema::TableSchema;
use sqlite_rs::vdbe::{execute_with_writable_db, ExecError};
use sqlite_rs::vfs::{UnixVfs, Vfs};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-codegen-delete-{label}-{}-{n}",
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

fn run_delete(
    path: &Path,
    header: &DatabaseHeader,
    page_size: u32,
    sql: &str,
    schema: &TableSchema,
) -> Result<(), ExecError> {
    let delete = match parse_delete(sql) {
        ParseOutcome::Accepted(d) => *d,
        other => panic!("failed to parse {sql:?}: {other:?}"),
    };
    let program = compile_delete(&delete, schema).unwrap();
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

#[test]
fn unconditional_delete_removes_every_row() {
    let path = scratch_db("unconditional");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);
    let schema = schema("CREATE TABLE t(a INTEGER, b TEXT)");

    for i in 1..=3 {
        run_insert(
            &path,
            &header,
            page_size,
            &format!("INSERT INTO t(a, b) VALUES ({i}, 'v{i}')"),
            &schema,
        );
    }
    assert_eq!(rows(&path, &header, page_size, 1).len(), 3);

    run_delete(&path, &header, page_size, "DELETE FROM t", &schema).unwrap();

    assert!(rows(&path, &header, page_size, 1).is_empty());
}

#[test]
fn where_filtered_delete_removes_only_matching_rows_mid_scan() {
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

    // Rows 2 and 4 sit between surviving rows on the same leaf page —
    // exercises delete-during-scan without skipping the row right
    // after a just-deleted one.
    run_delete(
        &path,
        &header,
        page_size,
        "DELETE FROM t WHERE a = 2 OR a = 4",
        &schema,
    )
    .unwrap();

    let got = rows(&path, &header, page_size, 1);
    let remaining: Vec<i64> = got.iter().map(|(rowid, _)| *rowid).collect();
    assert_eq!(remaining, vec![1, 3, 5]);
}

#[test]
fn delete_matching_nothing_leaves_all_rows_untouched() {
    let path = scratch_db("no-match");
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

    run_delete(
        &path,
        &header,
        page_size,
        "DELETE FROM t WHERE a = 999",
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

/// #336 regression: `WHERE rowid = <int literal>` compiles to
/// `SeekRowid`, not a `Rewind`/`Next` scan.
#[test]
fn rowid_equality_delete_compiles_to_seek_rowid() {
    let schema = schema("CREATE TABLE t(a INTEGER, b TEXT)");
    let delete = match parse_delete("DELETE FROM t WHERE rowid = 2") {
        ParseOutcome::Accepted(d) => *d,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_delete(&delete, &schema).unwrap();
    let rows = sqlite_rs::vdbe::explain(&program);
    assert!(
        rows.iter().any(|r| r.opcode == "SeekRowid"),
        "expected SeekRowid in the compiled program: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.opcode == "Rewind"),
        "rowid-equality delete must not also emit a full scan: {rows:?}"
    );
}

/// #336 regression: a compound `WHERE` (even one built entirely from
/// rowid-alias equalities) must NOT take the seek fast path — only a
/// single top-level equality does, matching #137's own narrow scope for
/// `SELECT`. Uses `a` as the table's `INTEGER PRIMARY KEY` rowid alias
/// (rather than the bare `rowid` keyword) so the fallback scan's
/// ordinary column resolution can actually compile `WHERE a = ...` —
/// resolving the bare `rowid` keyword itself outside the seek fast
/// paths is a separate, pre-existing gap this test isn't about.
#[test]
fn compound_where_with_rowid_alias_equality_falls_back_to_scan() {
    let schema = schema("CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT)");
    let delete = match parse_delete("DELETE FROM t WHERE a = 2 OR a = 5") {
        ParseOutcome::Accepted(d) => *d,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_delete(&delete, &schema).unwrap();
    let rows = sqlite_rs::vdbe::explain(&program);
    assert!(
        rows.iter().any(|r| r.opcode == "Rewind"),
        "expected the compound WHERE to still fall back to a full scan: {rows:?}"
    );
}

#[test]
fn rowid_equality_delete_removes_only_the_matching_row() {
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

    run_delete(
        &path,
        &header,
        page_size,
        "DELETE FROM t WHERE rowid = 3",
        &schema,
    )
    .unwrap();

    let got = rows(&path, &header, page_size, 1);
    let remaining: Vec<i64> = got.iter().map(|(rowid, _)| *rowid).collect();
    assert_eq!(remaining, vec![1, 2, 4, 5]);
}

/// A rowid that doesn't exist: the seek misses cleanly, no row deleted.
#[test]
fn rowid_equality_delete_with_no_such_rowid_is_a_no_op() {
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

    run_delete(
        &path,
        &header,
        page_size,
        "DELETE FROM t WHERE rowid = 999",
        &schema,
    )
    .unwrap();

    assert_eq!(rows(&path, &header, page_size, 1).len(), 1);
}
