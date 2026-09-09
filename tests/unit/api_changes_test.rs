// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! The rows-affected count, at the connection level (spec 013
//! Requirement 1).
//!
//! Spec 013 calls this the one item on its list a consumer cannot work
//! around. The engine half is `StepOutcome::changes` (#692); what this
//! covers is the *connection*'s rule, which is the half with the surprising
//! semantics: `sqlite3_changes()` is not "what the last statement did", it
//! is "what the last *counting* statement did". A `SELECT` in between must
//! leave it alone.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sqlite_rs::api::{Connection, Error};
use sqlite_rs::record::Value;

fn seeded() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t(id INTEGER, metadata_location TEXT);
         INSERT INTO t VALUES (1, 'a');
         INSERT INTO t VALUES (2, 'a');",
    )
    .unwrap();
    conn
}

/// The optimistic-concurrency case, and the reason the requirement exists:
/// *SQE* swaps a table's metadata pointer with a conditional `UPDATE` and
/// treats zero rows affected as a lost race.
#[test]
fn conditional_update_reports_match() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t(id INTEGER, metadata_location TEXT);
         INSERT INTO t VALUES (1, 'a');",
    )
    .unwrap();

    let swap = "UPDATE t SET metadata_location = 'b' WHERE metadata_location = 'a'";

    // First swap wins.
    assert_eq!(conn.execute(swap).unwrap(), 1);
    assert_eq!(conn.changes().unwrap(), 1);

    // The identical statement now matches nothing — a lost race, and it has
    // to be distinguishable from the win above.
    assert_eq!(conn.execute(swap).unwrap(), 0);
    assert_eq!(conn.changes().unwrap(), 0);
}

/// `Some(0)` and "not a counting statement" are different answers, and this
/// is where the difference shows.
#[test]
fn select_does_not_clobber_count() {
    let conn = seeded();

    assert_eq!(conn.execute("DELETE FROM t").unwrap(), 2);
    assert_eq!(conn.changes().unwrap(), 2);

    // A SELECT that returns no rows at all must not reset the count.
    conn.execute("SELECT id FROM t").unwrap();
    assert_eq!(
        conn.changes().unwrap(),
        2,
        "a SELECT clobbered the rows-changed count"
    );

    // Nor does DDL.
    conn.execute("CREATE TABLE u(x)").unwrap();
    assert_eq!(
        conn.changes().unwrap(),
        2,
        "DDL clobbered the rows-changed count"
    );
}

#[test]
fn insert_and_delete_report_their_rows() {
    let conn = seeded();
    assert_eq!(conn.execute("INSERT INTO t VALUES (3, 'c')").unwrap(), 1);
    assert_eq!(conn.execute("DELETE FROM t WHERE id < 3").unwrap(), 2);
    assert_eq!(conn.execute("DELETE FROM t").unwrap(), 1);
    assert_eq!(conn.execute("DELETE FROM t").unwrap(), 0);
}

/// Index maintenance is not a row change: the same statement against the
/// same table must report the same number whether or not indexes exist.
#[test]
fn indexes_do_not_inflate_the_count() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t(a INTEGER, b TEXT, c TEXT);
         CREATE INDEX t_a ON t(a);
         CREATE UNIQUE INDEX t_c ON t(c);",
    )
    .unwrap();

    assert_eq!(
        conn.execute("INSERT INTO t VALUES (1, 'b', 'c')").unwrap(),
        1
    );
    assert_eq!(conn.execute("UPDATE t SET b = 'z' WHERE a = 1").unwrap(), 1);
    assert_eq!(conn.execute("DELETE FROM t WHERE a = 1").unwrap(), 1);
}

#[test]
fn last_insert_rowid_is_retained_across_statements() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE u(id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    assert_eq!(
        conn.last_insert_rowid().unwrap(),
        0,
        "a connection that has inserted nothing should report 0"
    );

    conn.execute("INSERT INTO u(id, v) VALUES (42, 'a')")
        .unwrap();
    assert_eq!(conn.last_insert_rowid().unwrap(), 42);

    // Neither an UPDATE, a DELETE nor a SELECT may move it.
    conn.execute("UPDATE u SET v = 'b' WHERE id = 42").unwrap();
    assert_eq!(conn.last_insert_rowid().unwrap(), 42);
    conn.execute("DELETE FROM u WHERE id = 42").unwrap();
    assert_eq!(conn.last_insert_rowid().unwrap(), 42);
    conn.execute("SELECT id FROM u").unwrap();
    assert_eq!(conn.last_insert_rowid().unwrap(), 42);
}

/// Bound parameters on a write — the shape six of *SQE*'s eight statements
/// take, and the one combination the engine had no entry point for before
/// this facade (`execute_with_db_and_params` is read-only;
/// `execute_transaction_step` takes no parameters).
#[test]
fn a_parameterised_write_binds_and_counts() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t(a INTEGER, b TEXT)").unwrap();

    assert_eq!(
        conn.execute_with(
            "INSERT INTO t VALUES (?1, ?2)",
            vec![Value::Integer(1), Value::from("x")],
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.execute_with(
            "UPDATE t SET b = ?1 WHERE a = ?2",
            vec![Value::from("y"), Value::Integer(1)],
        )
        .unwrap(),
        1
    );
    // A miss reports zero rather than failing.
    assert_eq!(
        conn.execute_with(
            "UPDATE t SET b = ?1 WHERE a = ?2",
            vec![Value::from("z"), Value::Integer(999)],
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.execute_with("DELETE FROM t WHERE a = ?1", vec![Value::Integer(1)])
            .unwrap(),
        1
    );
}

/// Stricter than stock SQLite, which leaves an unbound parameter NULL. The
/// divergence is the point: a short or transposed argument list is what
/// Requirement 3 exists to catch.
#[test]
fn the_wrong_number_of_parameters_is_refused() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t(a INTEGER, b TEXT)").unwrap();

    let too_few = conn
        .execute_with("INSERT INTO t VALUES (?1, ?2)", vec![Value::Integer(1)])
        .expect_err("two placeholders, one value");
    assert_eq!(
        too_few,
        Error::ParamCount {
            expected: 2,
            found: 1
        }
    );
    assert_eq!(too_few.sqlite_code(), 25, "should report SQLITE_RANGE");

    let too_many = conn
        .execute_with(
            "INSERT INTO t VALUES (?1, ?2)",
            vec![Value::Integer(1), Value::from("x"), Value::Integer(3)],
        )
        .expect_err("two placeholders, three values");
    assert_eq!(
        too_many,
        Error::ParamCount {
            expected: 2,
            found: 3
        }
    );

    // Nothing was written by either attempt.
    conn.execute("DELETE FROM t").unwrap();
    assert_eq!(conn.changes().unwrap(), 0);
}

#[test]
fn a_named_parameter_is_refused_with_its_own_variant() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t(a INTEGER)").unwrap();

    let err = conn
        .execute("SELECT a FROM t WHERE a = :id")
        .expect_err("named parameters are not supported");
    assert_eq!(
        err,
        Error::NamedParameter {
            placeholder: ":id".to_string()
        },
        "should report the placeholder, not a generic compile failure"
    );
    assert_eq!(err.sqlite_code(), 1, "should report SQLITE_ERROR");
}

/// A UNIQUE violation has to arrive with SQLite's own result code, so a
/// consumer can tell it from any other failure without matching on text.
#[test]
fn a_constraint_violation_carries_the_sqlite_result_code() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t(a INTEGER, c TEXT);
         CREATE UNIQUE INDEX t_c ON t(c);
         INSERT INTO t VALUES (1, 'dup');",
    )
    .unwrap();

    let err = conn
        .execute("INSERT INTO t VALUES (2, 'dup')")
        .expect_err("duplicate value in a UNIQUE index");

    match err {
        Error::Sqlite { code, .. } => {
            // SQLITE_CONSTRAINT_UNIQUE = 19 | (8<<8), sqlite3.h at 3.53.4.
            assert_eq!(code, 2067, "expected SQLITE_CONSTRAINT_UNIQUE");
        }
        other => panic!("expected Error::Sqlite, got {other:?}"),
    }
    assert_eq!(
        err.sqlite_code(),
        19,
        "the primary code should be SQLITE_CONSTRAINT"
    );
    assert_eq!(err.extended_sqlite_code(), 2067);
    assert!(
        !err.is_retryable(),
        "a constraint violation is not retryable"
    );
}
