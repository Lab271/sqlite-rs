// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! #708: `rowid`/`_rowid_`/`oid` were resolvable in a `WHERE` clause
//! (`is_rowid_reference`'s seek fast path) but not in a projection —
//! `Scope::resolve` had no pseudo-column awareness. Oracle-diffed
//! against the pinned 3.53.4 `sqlite3`, reusing
//! `tests/unit/codegen_select_test.rs`'s scratch-db-plus-oracle
//! pattern, but through the real `oracle::pinned_oracle()` (never the
//! system `/usr/bin/sqlite3`).

#[path = "../corpus/oracle.rs"]
#[allow(dead_code)]
mod oracle;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use oracle::pinned_oracle;
use sqlite_rs::codegen::compile_select;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Value;
use sqlite_rs::schema::TableSchema;
use sqlite_rs::vdbe::execute_with_db;
use sqlite_rs::vfs::{UnixVfs, Vfs, VfsPageSource};

fn scratch_db(label: &str, oracle: &Path, setup_sql: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_rowid_projection_test_{}_{}.db",
        std::process::id(),
        label
    ));
    std::fs::remove_file(&path).ok();
    let status = Command::new(oracle)
        .arg(&path)
        .arg(setup_sql)
        .status()
        .expect("creating scratch fixture db");
    assert!(status.success());
    path
}

fn our_rows(path: &Path, schema: &TableSchema, sql: &str) -> Result<Vec<Vec<Value>>, String> {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => return Err(format!("parser rejected {sql:?}: {other:?}")),
    };
    let program = compile_select(&select, schema).map_err(|e| format!("{e:?}"))?;
    let vfs = UnixVfs;
    let file = vfs.open_read(path).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source = VfsPageSource::open(&vfs, path, header.page_size).unwrap();
    execute_with_db(&program, Rc::new(source), header).map_err(|e| format!("{e:?}"))
}

fn oracle_rows(oracle: &Path, db: &Path, sql: &str) -> Vec<Vec<String>> {
    let output = Command::new(oracle)
        .arg("-readonly")
        .arg("-separator")
        .arg("\u{1f}")
        .arg(db)
        .arg(sql)
        .output()
        .expect("invoking sqlite3 oracle");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.split('\u{1f}').map(str::to_string).collect())
        .collect()
}

fn value_to_oracle_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => {
            if r.fract() == 0.0 {
                format!("{r:.1}")
            } else {
                r.to_string()
            }
        }
        Value::Text(s) => s.to_string(),
        Value::Blob(_) => "<blob>".to_string(),
    }
}

fn assert_matches_oracle(oracle: &Path, db: &Path, schema: &TableSchema, sql: &str) {
    let ours = our_rows(db, schema, sql).unwrap_or_else(|e| panic!("compiling {sql:?}: {e}"));
    let ours_text: Vec<Vec<String>> = ours
        .iter()
        .map(|row| row.iter().map(value_to_oracle_text).collect())
        .collect();
    let expected = oracle_rows(oracle, db, sql);
    assert_eq!(ours_text, expected, "mismatch for {sql:?}");
}

fn rowid_table_schema() -> TableSchema {
    TableSchema {
        unresolved_autoindex: false,
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["a".to_string(), "v".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        column_collations: vec![],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
        indexes: vec![],
        rowid_alias: None,
    }
    .with_computed_rowid_alias()
}

/// The core defect: `rowid` in a projection alongside real columns.
/// Also exercises gaps left by a delete (#708's acceptance criteria).
#[test]
fn rowid_selectable_in_projection_including_after_deletes() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "basic",
        &oracle,
        "CREATE TABLE t(a INTEGER, v TEXT); \
         INSERT INTO t VALUES (1, 'aa'), (2, 'bb'), (3, 'cc'), (4, 'dd'); \
         DELETE FROM t WHERE a = 2;",
    );
    let schema = rowid_table_schema();
    assert_matches_oracle(&oracle, &db, &schema, "SELECT rowid, a, v FROM t");
    assert_matches_oracle(&oracle, &db, &schema, "SELECT _rowid_, a FROM t");
    assert_matches_oracle(&oracle, &db, &schema, "SELECT oid FROM t");
}

/// `rowid` in a projection, WHERE and ORDER BY together, plus an
/// alias, in the same statement.
#[test]
fn rowid_works_in_where_order_by_and_alias_together() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "combo",
        &oracle,
        "CREATE TABLE t(a INTEGER, v TEXT); \
         INSERT INTO t VALUES (1, 'aa'), (2, 'bb'), (3, 'cc');",
    );
    let schema = rowid_table_schema();
    assert_matches_oracle(
        &oracle,
        &db,
        &schema,
        "SELECT rowid AS rid, a FROM t WHERE rowid > 1 ORDER BY rowid DESC",
    );
}

/// Shadowing: a declared column named `rowid` wins over the
/// pseudo-column.
#[test]
fn declared_rowid_column_shadows_the_pseudo_column() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "shadow",
        &oracle,
        "CREATE TABLE t(rowid TEXT, v INTEGER); \
         INSERT INTO t VALUES ('x', 1), ('y', 2);",
    );
    let schema = TableSchema {
        unresolved_autoindex: false,
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["rowid".to_string(), "v".to_string()],
        column_types: vec!["TEXT".to_string(), "INTEGER".to_string()],
        column_collations: vec![],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: String::new(),
        indexes: vec![],
        rowid_alias: None,
    }
    .with_computed_rowid_alias();
    assert_matches_oracle(&oracle, &db, &schema, "SELECT rowid, v FROM t");
}

/// `INTEGER PRIMARY KEY` tables: `rowid` and the alias column must
/// agree.
#[test]
fn integer_primary_key_alias_and_rowid_agree() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "ipk",
        &oracle,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT); \
         INSERT INTO t VALUES (5, 'aa'), (9, 'bb');",
    );
    let schema = TableSchema {
        unresolved_autoindex: false,
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["id".to_string(), "v".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        column_collations: vec![],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)".to_string(),
        indexes: vec![],
        rowid_alias: None,
    }
    .with_computed_rowid_alias();
    assert_matches_oracle(&oracle, &db, &schema, "SELECT rowid, id, v FROM t");
}

/// `WITHOUT ROWID` tables have no rowid at all — must be rejected the
/// same way the oracle rejects it (an unknown-column error), not
/// silently produce a number.
#[test]
fn without_rowid_table_rejects_rowid_reference() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "without_rowid",
        &oracle,
        "CREATE TABLE t(a INTEGER PRIMARY KEY, v TEXT) WITHOUT ROWID; \
         INSERT INTO t VALUES (1, 'aa');",
    );
    let oracle_out = Command::new(&oracle)
        .arg("-readonly")
        .arg(&db)
        .arg("SELECT rowid FROM t")
        .output()
        .expect("invoking sqlite3 oracle");
    assert!(
        !oracle_out.status.success(),
        "expected the oracle itself to reject rowid on a WITHOUT ROWID table"
    );

    let schema = TableSchema {
        unresolved_autoindex: false,
        name: "t".to_string(),
        root_page: 2,
        columns: vec!["a".to_string(), "v".to_string()],
        column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
        column_collations: vec![],
        without_rowid: true,
        strict: false,
        is_virtual: false,
        sql: "CREATE TABLE t(a INTEGER PRIMARY KEY, v TEXT) WITHOUT ROWID".to_string(),
        indexes: vec![],
        rowid_alias: None,
    }
    .with_computed_rowid_alias();
    let err = our_rows(&db, &schema, "SELECT rowid FROM t")
        .expect_err("rowid on a WITHOUT ROWID table must be rejected");
    assert!(
        err.contains("UnknownColumn"),
        "expected an unknown-column error, got: {err}"
    );
}
