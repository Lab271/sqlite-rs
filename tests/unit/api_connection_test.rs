// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Opening and creating a connection (spec 013 Requirement 2).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use sqlite_rs::api::{Connection, Error, OpenMode};

fn scratch(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("sqlite-rs-api-conn-{}-{label}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn clean(path: &Path) {
    if let Some(dir) = path.parent() {
        std::fs::remove_dir_all(dir).ok();
    }
}

#[test]
fn open_creates_a_database_that_reopens_and_reads_back() {
    let path = scratch("create");
    clean(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    assert!(!path.exists());

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE t(a INTEGER, b TEXT)").unwrap();
        conn.execute("INSERT INTO t VALUES (1, 'x')").unwrap();
    }
    assert!(path.exists(), "open should have created the file");

    // Reopening is the real check: the header we wrote has to parse, and
    // the catalog we wrote has to decode.
    let conn = Connection::open(&path).unwrap();
    assert_eq!(conn.execute("INSERT INTO t VALUES (2, 'y')").unwrap(), 1);

    clean(&path);
}

/// Requirement 2 is explicit that this creates nothing — so the assertion
/// is on the filesystem, not only on the error.
#[test]
fn readwrite_does_not_create() {
    let path = scratch("no-create");
    clean(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    assert!(!path.exists());

    let err = Connection::open_with(&path, OpenMode::ReadWrite)
        .expect_err("ReadWrite on a missing file should fail");

    assert!(
        matches!(err, Error::CannotOpen { .. }),
        "expected CannotOpen, got {err:?}"
    );
    assert_eq!(err.sqlite_code(), 14, "should report SQLITE_CANTOPEN");
    assert!(!err.is_retryable());
    assert!(
        !path.exists(),
        "ReadWrite must not bring the file into existence"
    );

    clean(&path);
}

#[test]
fn readonly_reads_but_refuses_every_write() {
    let path = scratch("readonly");
    clean(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE t(a INTEGER, b TEXT)").unwrap();
        conn.execute("INSERT INTO t VALUES (1, 'x')").unwrap();
    }

    let conn = Connection::open_with(&path, OpenMode::ReadOnly).unwrap();

    // Reads work, including one whose plan materializes a FROM-subquery.
    // That emits an `Insert` against an *ephemeral* cursor, so a read-only
    // guard keyed on `Insert` rather than `OpenWrite` would wrongly refuse
    // a plain `SELECT`. This is the case that pins the discriminator.
    //
    // The `LIMIT 5` is load-bearing and must not be "simplified" away:
    // without it `flatten_from_subqueries` folds the subquery into the
    // outer query and no ephemeral write is emitted at all. Measured on
    // this tree: with the LIMIT the program has Insert x1 / OpenWrite x0;
    // without it, Insert x0. Remove the LIMIT and this test still passes
    // while proving nothing.
    conn.execute("SELECT a FROM t").unwrap();
    conn.execute("SELECT s.a FROM (SELECT a FROM t LIMIT 5) AS s ORDER BY s.a")
        .unwrap();

    for sql in [
        "INSERT INTO t VALUES (2, 'y')",
        "UPDATE t SET b = 'z' WHERE a = 1",
        "DELETE FROM t WHERE a = 1",
        "CREATE TABLE u(x)",
        "CREATE INDEX t_a ON t(a)",
        "DROP TABLE t",
    ] {
        match conn.execute(sql) {
            Err(Error::ReadOnly { statement }) => assert_eq!(statement, sql),
            Err(other) => panic!("{sql} was refused, but as {other:?} not ReadOnly"),
            Ok(changed) => {
                panic!("{sql} was allowed on a read-only connection (changed {changed} rows)")
            }
        }
    }

    clean(&path);
}

#[test]
fn readonly_refusal_reports_sqlite_readonly() {
    let path = scratch("readonly-code");
    clean(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE t(a INTEGER)").unwrap();
    }

    let conn = Connection::open_with(&path, OpenMode::ReadOnly).unwrap();
    let err = conn
        .execute("INSERT INTO t VALUES (1)")
        .expect_err("refused");
    assert!(
        matches!(err, Error::ReadOnly { .. }),
        "expected ReadOnly, got {err:?}"
    );
    assert_eq!(err.sqlite_code(), 8, "should report SQLITE_READONLY");
    assert!(!err.is_retryable());

    // And nothing was written.
    let conn = Connection::open_with(&path, OpenMode::ReadWrite).unwrap();
    assert_eq!(conn.execute("DELETE FROM t").unwrap(), 0);

    clean(&path);
}

#[test]
fn an_in_memory_database_works_and_is_private() {
    let a = Connection::open_in_memory().unwrap();
    a.execute("CREATE TABLE t(a INTEGER)").unwrap();
    assert_eq!(a.execute("INSERT INTO t VALUES (1)").unwrap(), 1);

    // A second in-memory connection is a different database, not a shared
    // one — so `t` must not exist in it.
    let b = Connection::open_in_memory().unwrap();
    assert!(
        b.execute("INSERT INTO t VALUES (1)").is_err(),
        "in-memory databases should be private to their connection"
    );
}

#[test]
fn a_multi_statement_string_is_refused_by_execute() {
    let conn = Connection::open_in_memory().unwrap();
    let err = conn
        .execute("CREATE TABLE t(a INTEGER); CREATE TABLE u(b INTEGER)")
        .expect_err("execute takes exactly one statement");
    assert_eq!(err, Error::MultipleStatements { count: 2 });
    assert_eq!(err.sqlite_code(), 21, "should report SQLITE_MISUSE");

    // ...and execute_batch is the way to run it.
    conn.execute_batch("CREATE TABLE t(a INTEGER); CREATE TABLE u(b INTEGER)")
        .unwrap();
    conn.execute("INSERT INTO t VALUES (1)").unwrap();
    conn.execute("INSERT INTO u VALUES (2)").unwrap();
}

#[test]
fn a_syntax_error_reports_where_it_is() {
    let conn = Connection::open_in_memory().unwrap();
    let err = conn.execute("SELECT FROM").expect_err("not valid SQL");
    match err {
        Error::Parse { line, column, .. } => {
            assert_eq!(line, 1);
            assert!(column > 0, "column should be 1-based, got {column}");
        }
        other => panic!("expected a Parse error, got {other:?}"),
    }
}

#[test]
fn a_batch_stops_at_the_first_failure() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t(a INTEGER)").unwrap();

    let err = conn
        .execute_batch(
            "INSERT INTO t VALUES (1); INSERT INTO nope VALUES (2); INSERT INTO t VALUES (3)",
        )
        .expect_err("the middle statement targets no table");
    assert!(matches!(err, Error::Compile { .. }), "got {err:?}");

    // The statement before the failure stayed applied — execute_batch is
    // not a transaction, and says so.
    conn.execute("DELETE FROM t WHERE a = 1").unwrap();
    assert_eq!(conn.changes().unwrap(), 1);
    // The one after it never ran.
    conn.execute("DELETE FROM t WHERE a = 3").unwrap();
    assert_eq!(conn.changes().unwrap(), 0);
}
