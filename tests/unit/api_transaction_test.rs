// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Transactions on the embedding API (spec 013 Requirement 5).
//!
//! The property that matters is not that `BEGIN` works — the engine has had
//! that since #356 — but that a transaction *handle* cannot be left open by
//! accident. A `?` returning early out of a function holding one must roll
//! back, because the alternative for a consumer storing pointers to data it
//! cannot otherwise find is a half-applied catalog change.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use sqlite_rs::api::{Connection, Error, TransactionBehavior, Value};

fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sqlite-rs-api-tx-{}-{label}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn clean(path: &Path) {
    if let Some(dir) = path.parent() {
        std::fs::remove_dir_all(dir).ok();
    }
}

fn count(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM t")
        .unwrap()
        .expect("count returns a row")
        .get(0)
        .unwrap()
}

fn seeded() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES (1, 'x');")
        .unwrap();
    conn
}

#[test]
fn a_committed_transaction_keeps_its_writes() {
    let conn = seeded();
    let tx = conn.transaction().unwrap();
    tx.execute("INSERT INTO t VALUES (2, 'y')").unwrap();
    tx.execute("INSERT INTO t VALUES (3, 'z')").unwrap();
    tx.commit().unwrap();

    assert_eq!(count(&conn), 3);
}

/// Requirement 5's first scenario.
#[test]
fn drop_rolls_back() {
    let conn = seeded();
    {
        let tx = conn.transaction().unwrap();
        tx.execute("INSERT INTO t VALUES (2, 'y')").unwrap();
        assert_eq!(count(&tx), 2, "the write is visible inside the transaction");
        // Dropped without commit.
    }
    assert_eq!(
        count(&conn),
        1,
        "the dropped transaction should have rolled back"
    );
}

#[test]
fn an_explicit_rollback_reports_its_errors() {
    let conn = seeded();
    let tx = conn.transaction().unwrap();
    tx.execute("INSERT INTO t VALUES (2, 'y')").unwrap();
    tx.rollback().unwrap();

    assert_eq!(count(&conn), 1);
}

/// The reason the type exists: an early return must not leave a
/// transaction open, and the next statement must not silently join it.
#[test]
fn an_early_return_rolls_back_and_leaves_the_connection_usable() {
    let conn = seeded();

    fn fallible(conn: &Connection) -> Result<(), Error> {
        let tx = conn.transaction()?;
        tx.execute("INSERT INTO t VALUES (2, 'y')")?;
        // Fails: no such table. The `?` returns while `tx` is live.
        tx.execute("INSERT INTO nope VALUES (1)")?;
        tx.commit()
    }

    assert!(fallible(&conn).is_err());
    assert_eq!(
        count(&conn),
        1,
        "the failed unit of work should be entirely absent"
    );

    // And the connection is back in autocommit, not stuck in a transaction.
    conn.execute("INSERT INTO t VALUES (9, 'ok')").unwrap();
    assert_eq!(count(&conn), 2);
}

#[test]
fn all_three_behaviours_begin_and_commit() {
    for behavior in [
        TransactionBehavior::Deferred,
        TransactionBehavior::Immediate,
        TransactionBehavior::Exclusive,
    ] {
        let conn = seeded();
        let tx = conn.transaction_with(behavior).unwrap();
        tx.execute("INSERT INTO t VALUES (2, 'y')").unwrap();
        tx.commit()
            .unwrap_or_else(|e| panic!("{behavior:?} failed to commit: {e}"));
        assert_eq!(count(&conn), 2, "{behavior:?} lost its write");
    }
}

/// Nesting is not supported, and the refusal must be an error rather than a
/// silently flattened second transaction.
#[test]
fn a_nested_transaction_is_refused() {
    let conn = seeded();
    let _outer = conn.transaction().unwrap();
    let inner = conn.transaction();
    assert!(
        inner.is_err(),
        "a second BEGIN while one is open should be refused"
    );
}

/// A transaction spanning several statements is one unit on disk, checked
/// by reopening the file rather than by asking the same connection.
#[test]
fn a_transaction_is_one_unit_on_disk() {
    let path = scratch("unit");
    clean(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE t(a INTEGER, b TEXT)").unwrap();

        let tx = conn.transaction().unwrap();
        for i in 0..20 {
            tx.execute_with(
                "INSERT INTO t VALUES (?1, ?2)",
                vec![Value::from(i), Value::from("x")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }

    let reopened = Connection::open(&path).unwrap();
    assert_eq!(count(&reopened), 20);

    // And a rolled-back one leaves nothing behind on disk either.
    {
        let tx = reopened.transaction().unwrap();
        tx.execute("INSERT INTO t VALUES (999, 'gone')").unwrap();
        drop(tx);
    }
    drop(reopened);

    let reopened = Connection::open(&path).unwrap();
    assert_eq!(count(&reopened), 20, "a rolled-back write reached the file");

    clean(&path);
}

#[test]
fn pragma_sets_a_value_the_engine_honours() {
    let path = scratch("pragma");
    clean(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();

    let conn = Connection::open(&path).unwrap();
    // Both levels of the durability knob Requirement 5 names.
    conn.pragma("synchronous", "FULL").unwrap();
    conn.pragma("synchronous", "OFF").unwrap();
    conn.pragma("synchronous", "NORMAL").unwrap();

    // And the statement still works afterwards.
    conn.execute("CREATE TABLE t(a INTEGER, b TEXT)").unwrap();
    conn.execute("INSERT INTO t VALUES (1, 'x')").unwrap();
    assert_eq!(count(&conn), 1);

    clean(&path);
}

#[test]
fn the_busy_timeout_is_settable() {
    let conn = Connection::open_in_memory().unwrap();
    // Default is zero (stock SQLite's), and any value is accepted.
    conn.set_busy_timeout(std::time::Duration::from_millis(250))
        .unwrap();
    conn.set_busy_timeout(std::time::Duration::ZERO).unwrap();

    // Setting it does not disturb the connection.
    conn.execute("CREATE TABLE t(a INTEGER, b TEXT)").unwrap();
    conn.execute("INSERT INTO t VALUES (1, 'x')").unwrap();
    assert_eq!(count(&conn), 1);
}

/// The counters follow the transaction: a rolled-back `INSERT` still
/// *reported* its row, because `sqlite3_changes()` counts what a statement
/// did rather than what survived. Pinned so a future change to the rollback
/// path does not quietly redefine it.
#[test]
fn a_rolled_back_statement_still_reported_its_count() {
    let conn = seeded();
    {
        let tx = conn.transaction().unwrap();
        assert_eq!(tx.execute("INSERT INTO t VALUES (2, 'y')").unwrap(), 1);
        assert_eq!(tx.changes().unwrap(), 1);
    }
    // The row is gone...
    assert_eq!(count(&conn), 1);
    // ...but the count is not retroactively revised, matching SQLite.
    assert_eq!(conn.changes().unwrap(), 1);
}
