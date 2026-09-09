// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Prepared statements (spec 013 Requirement 3) and schema refresh
//! (Requirement 8).
//!
//! Requirement 3 is explicit that the value here is not speed — a dozen
//! statements at commit frequency saves nothing measurable by compiling
//! once. It is that a handle owning its parameter slots refuses a wrong
//! argument count instead of writing a valid row that points at the wrong
//! table.
//!
//! Requirement 8 is the sharper one. A program addresses tables by root
//! page, and a `DROP` can return that page to the freelist for a later
//! `CREATE` to reuse — so a statement compiled before a schema change and
//! run after it could read a page that now belongs to a different table.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sqlite_rs::api::{Connection, Error};
use sqlite_rs::record::Value;

fn seeded() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE t(id INTEGER, name TEXT);
         INSERT INTO t VALUES (1, 'one');
         INSERT INTO t VALUES (2, 'two');
         INSERT INTO t VALUES (3, 'three');",
    )
    .unwrap();
    conn
}

/// Requirement 3's first scenario.
#[test]
fn compile_once_bind_many() {
    let conn = seeded();
    let stmt = conn.prepare("SELECT name FROM t WHERE id = ?1").unwrap();
    assert_eq!(stmt.param_count(), 1);

    for (id, expected) in [(1i64, "one"), (2, "two"), (3, "three")] {
        let row = stmt
            .query_row(vec![Value::from(id)])
            .unwrap()
            .unwrap_or_else(|| panic!("id {id} should match a row"));
        assert_eq!(row.get::<String>(0).unwrap(), expected);
    }

    // "...and compilation happened once". Observable rather than asserted
    // about internals: `reprepare_count` is SQLite's
    // SQLITE_STMTSTATUS_REPREPARE, the number of automatic recompiles.
    assert_eq!(
        stmt.reprepare_count().unwrap(),
        0,
        "the statement was recompiled despite the schema holding still"
    );
}

/// Requirement 3's second scenario.
#[test]
fn named_param_is_refused_at_prepare() {
    let conn = seeded();
    let err = conn
        .prepare("SELECT * FROM t WHERE id = :id")
        .expect_err("a named parameter should be refused at prepare time");

    assert_eq!(
        err,
        Error::NamedParameter {
            placeholder: ":id".to_string()
        },
        "the refusal should name the unsupported form"
    );
}

#[test]
fn a_prepared_write_binds_and_counts() {
    let conn = seeded();
    let insert = conn.prepare("INSERT INTO t VALUES (?1, ?2)").unwrap();
    assert_eq!(insert.param_count(), 2);

    assert_eq!(
        insert
            .execute(vec![Value::from(4), Value::from("four")])
            .unwrap(),
        1
    );
    assert_eq!(
        insert
            .execute(vec![Value::from(5), Value::from("five")])
            .unwrap(),
        1
    );

    let update = conn
        .prepare("UPDATE t SET name = ?1 WHERE id = ?2")
        .unwrap();
    assert_eq!(
        update
            .execute(vec![Value::from("IV"), Value::from(4)])
            .unwrap(),
        1
    );
    // A miss reports zero, which is the optimistic-concurrency signal.
    assert_eq!(
        update
            .execute(vec![Value::from("x"), Value::from(999)])
            .unwrap(),
        0
    );

    let count: i64 = conn
        .query_row("SELECT count(*) FROM t")
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(count, 5);
}

/// The stated reason the type exists: a wrong argument count is refused
/// rather than silently bound to NULL.
#[test]
fn a_wrong_argument_count_is_refused_every_time() {
    let conn = seeded();
    let stmt = conn.prepare("INSERT INTO t VALUES (?1, ?2)").unwrap();

    assert_eq!(
        stmt.execute(vec![Value::from(9)]).expect_err("too few"),
        Error::ParamCount {
            expected: 2,
            found: 1
        }
    );
    assert_eq!(
        stmt.execute(vec![Value::from(9), Value::from("a"), Value::from("b")])
            .expect_err("too many"),
        Error::ParamCount {
            expected: 2,
            found: 3
        }
    );

    // The refusals wrote nothing, and the handle still works afterwards.
    assert_eq!(
        stmt.execute(vec![Value::from(9), Value::from("nine")])
            .unwrap(),
        1
    );
    let count: i64 = conn
        .query_row("SELECT count(*) FROM t")
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(count, 4);
}

#[test]
fn column_names_are_known_before_the_statement_runs() {
    let conn = seeded();
    let stmt = conn.prepare("SELECT id, name FROM t").unwrap();
    assert_eq!(stmt.column_names(), ["id", "name"]);
}

#[test]
fn a_prepared_statement_streams_like_an_ad_hoc_one() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t(a INTEGER)").unwrap();
    let insert = conn.prepare("INSERT INTO t VALUES (?1)").unwrap();
    for i in 0..200 {
        insert.execute(vec![Value::from(i)]).unwrap();
    }

    let select = conn.prepare("SELECT a FROM t").unwrap();
    let mut rows = select.query(vec![]).unwrap();
    let mut seen = 0i64;
    while let Some(row) = rows.next_row().unwrap() {
        assert_eq!(row.get::<i64>(0).unwrap(), seen);
        seen += 1;
    }
    assert_eq!(
        seen, 200,
        "a prepared query should span batch boundaries too"
    );
}

/// Requirement 8. A statement prepared before a schema change must not run
/// against the old plan.
///
/// The recompile is what `sqlite3_prepare_v2` does on `SQLITE_SCHEMA`, and
/// `reprepare_count` is how it is observed.
#[test]
fn a_statement_recompiles_after_a_schema_change() {
    let conn = seeded();
    let stmt = conn.prepare("SELECT name FROM t WHERE id = ?1").unwrap();

    let before = stmt.query_row(vec![Value::from(1)]).unwrap().unwrap();
    assert_eq!(before.get::<String>(0).unwrap(), "one");
    assert_eq!(stmt.reprepare_count().unwrap(), 0);

    // A schema change that does not affect this statement's own table, but
    // does move the catalog.
    conn.execute("CREATE TABLE other(x INTEGER)").unwrap();

    let after = stmt.query_row(vec![Value::from(2)]).unwrap().unwrap();
    assert_eq!(
        after.get::<String>(0).unwrap(),
        "two",
        "the statement still works"
    );
    assert_eq!(
        stmt.reprepare_count().unwrap(),
        1,
        "the statement should have recompiled against the new catalog"
    );

    // And it does not recompile again while the schema holds still.
    stmt.query_row(vec![Value::from(3)]).unwrap().unwrap();
    assert_eq!(stmt.reprepare_count().unwrap(), 1);
}

/// The case Requirement 8 is really about: an index created after the
/// statement was prepared changes the plan, and a stale program would
/// neither use it nor maintain it.
#[test]
fn an_index_created_after_prepare_is_picked_up() {
    let conn = seeded();
    let select = conn.prepare("SELECT name FROM t WHERE id = ?1").unwrap();
    select.query_row(vec![Value::from(1)]).unwrap().unwrap();

    conn.execute("CREATE INDEX t_id ON t(id)").unwrap();

    // Still correct after the plan changes under it.
    let row = select.query_row(vec![Value::from(2)]).unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "two");

    // A prepared write must maintain the new index. This asserts only that
    // the row is *reachable* — deliberately not claimed as proof of index
    // maintenance, because the planner may satisfy this read with a table
    // scan and then find the row whether the index has it or not. Measured:
    // with the refresh disabled, this assertion still passes.
    //
    // The real claim needs a third party that validates the index against
    // the table, so it lives where the oracle does:
    // `tests/corpus/api_oracle_test.rs::a_prepared_write_after_create_index_keeps_the_file_valid`
    // inserts through a statement prepared before the index existed and has
    // the pinned sqlite3 run `PRAGMA integrity_check`.
    let insert = conn.prepare("INSERT INTO t VALUES (?1, ?2)").unwrap();
    insert
        .execute(vec![Value::from(4), Value::from("four")])
        .unwrap();

    let via_index = conn
        .query_row("SELECT name FROM t WHERE id = 4")
        .unwrap()
        .expect("the new row should be findable through the index");
    assert_eq!(via_index.get::<String>(0).unwrap(), "four");

    assert!(
        select.reprepare_count().unwrap() >= 1,
        "the statement should have recompiled once the index appeared"
    );
}

/// If the statement's own table is dropped, recompiling fails — and the
/// error must be reported rather than the old program silently reused.
#[test]
fn a_statement_whose_table_is_dropped_reports_the_failure() {
    let conn = seeded();
    let stmt = conn.prepare("SELECT name FROM t WHERE id = ?1").unwrap();
    stmt.query_row(vec![Value::from(1)]).unwrap().unwrap();

    conn.execute("DROP TABLE t").unwrap();

    let err = stmt
        .query_row(vec![Value::from(1)])
        .expect_err("the table is gone; the old program must not be reused");
    assert!(
        matches!(err, Error::Compile { .. } | Error::Parse { .. }),
        "expected a compile failure, got {err:?}"
    );

    // Repeatable rather than a one-shot: the handle stays valid and keeps
    // reporting the same thing.
    assert!(stmt.query_row(vec![Value::from(1)]).is_err());
}

#[test]
fn statements_outlive_the_connection_handle_they_were_made_from() {
    let stmt = {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t(a INTEGER)").unwrap();
        conn.prepare("INSERT INTO t VALUES (?1)").unwrap()
        // `conn` dropped here; the statement holds its own clone.
    };

    assert_eq!(stmt.execute(vec![Value::from(1)]).unwrap(), 1);
    let count: i64 = stmt
        .connection()
        .query_row("SELECT count(*) FROM t")
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn many_statements_coexist_and_are_finalized_independently() {
    let conn = seeded();
    let a = conn.prepare("SELECT name FROM t WHERE id = ?1").unwrap();
    let b = conn.prepare("SELECT count(*) FROM t").unwrap();
    let c = conn.prepare("INSERT INTO t VALUES (?1, ?2)").unwrap();

    assert_eq!(
        a.query_row(vec![Value::from(1)])
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "one"
    );
    drop(a);

    // Dropping one must not disturb the others.
    assert_eq!(
        c.execute(vec![Value::from(4), Value::from("four")])
            .unwrap(),
        1
    );
    assert_eq!(
        b.query_row(vec![]).unwrap().unwrap().get::<i64>(0).unwrap(),
        4
    );
}

#[test]
fn prepare_refuses_a_multi_statement_string() {
    let conn = seeded();
    let err = conn
        .prepare("SELECT 1 FROM t; SELECT 2 FROM t")
        .expect_err("prepare takes exactly one statement");
    assert_eq!(err, Error::MultipleStatements { count: 2 });
}
