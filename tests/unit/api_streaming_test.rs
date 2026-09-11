// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Incremental row access (spec 013 Requirement 7).
//!
//! `execute_with_db` and friends return `Vec<Vec<Value>>`, so the engine
//! allocates the whole result before the caller sees row 0. Spike 014
//! (#682) measured that at 137.7 MB peak heap and 5.36 ms to first row for
//! a 1,000,000-row result, against 8.68 MB and 44.7 µs streamed.
//!
//! What these tests pin is the property the spec settled on after the
//! original wording turned out to be unsatisfiable: peak cost is
//! **independent of the result size**, not proportional to the rows pulled.
//! A streaming read's memory is dominated by the pager's page cache, so it
//! is a floor rather than a slope — and independence is testable where
//! proportionality is not.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Instant;

use sqlite_rs::api::{Connection, Error, Value};

fn seeded(rows: i64) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t(a INTEGER, b TEXT)").unwrap();
    conn.execute("BEGIN").ok();
    for i in 0..rows {
        conn.execute_with(
            "INSERT INTO t VALUES (?1, ?2)",
            vec![Value::from(i), Value::from(format!("row-{i}"))],
        )
        .unwrap();
    }
    conn.execute("COMMIT").ok();
    conn
}

#[test]
fn rows_come_back_in_order_with_their_values() {
    let conn = seeded(10);
    let mut rows = conn.query("SELECT a, b FROM t ORDER BY a").unwrap();

    for expected in 0..10i64 {
        let row = rows.next_row().unwrap().expect("a row");
        assert_eq!(row.get::<i64>(0).unwrap(), expected);
        assert_eq!(row.get::<String>(1).unwrap(), format!("row-{expected}"));
    }
    assert!(rows.next_row().unwrap().is_none(), "should be exhausted");
    // Polling past the end keeps answering None rather than erroring.
    assert!(rows.next_row().unwrap().is_none());
}

#[test]
fn column_names_are_available_before_the_first_row() {
    let conn = seeded(3);
    let rows = conn.query("SELECT a, b FROM t ORDER BY a").unwrap();
    assert_eq!(rows.column_names(), ["a", "b"]);
}

/// A result larger than one channel batch, so the batching path is
/// exercised rather than only the single-chunk case.
#[test]
fn a_result_spanning_many_batches_is_complete_and_ordered() {
    let conn = seeded(500);
    let mut rows = conn.query("SELECT a FROM t ORDER BY a").unwrap();
    let mut seen = 0i64;
    while let Some(row) = rows.next_row().unwrap() {
        assert_eq!(row.get::<i64>(0).unwrap(), seen);
        seen += 1;
    }
    assert_eq!(seen, 500);
}

/// Requirement 7's first scenario, as the amended spec states it: the cost
/// of reading ten rows must not grow with the table.
///
/// Timing rather than heap, because a portable allocator hook is not
/// available here and the spike already measured the heap directly (#682:
/// 137.7 MB materialized against 8.68 MB streamed at 1,000,000 rows). Time
/// to first row is the observable that would degrade if the engine
/// materialized: it would have to build every row before handing over row
/// 0, so it would scale with the table while a streamed read stays flat.
///
/// **The plan must be non-blocking, and the query here is chosen for that.**
/// An earlier draft used `SELECT a FROM t ORDER BY a` and failed at 33x on
/// a 50x larger table — correctly. `ORDER BY` with no usable index is a
/// blocking operator: the sorter consumes every row before emitting the
/// first, so time-to-first-row is genuinely linear and no amount of
/// streaming changes that. Stock SQLite behaves identically. Testing the
/// streaming property therefore requires a plan that can emit as it scans,
/// which is what a bare scan is. `blocking_plans_are_linear_by_nature`
/// below pins the contrast so this is documented rather than merely
/// avoided.
///
/// The bound is deliberately loose (a 20x allowance over a 50x size
/// increase). This is a *shape* assertion — flat versus linear — and a
/// tight threshold on a shared machine would be flaky without testing
/// anything more.
#[test]
fn partial_read_is_bounded() {
    let first_ten = |conn: &Connection| -> (std::time::Duration, Vec<i64>) {
        let start = Instant::now();
        // No ORDER BY: rowid order already ascends by `a` here, and a bare
        // scan can emit its first row without reading the last.
        let mut rows = conn.query("SELECT a FROM t").unwrap();
        let mut out = Vec::new();
        for _ in 0..10 {
            match rows.next_row().unwrap() {
                Some(row) => out.push(row.get::<i64>(0).unwrap()),
                None => break,
            }
        }
        let elapsed = start.elapsed();
        // Dropped undrained, after the clock stops: the rest of the result
        // is abandoned and the worker is freed.
        drop(rows);
        (elapsed, out)
    };

    let small = seeded(200);
    let large = seeded(10_000);

    let (small_time, small_rows) = first_ten(&small);
    let (large_time, large_rows) = first_ten(&large);

    assert_eq!(small_rows, (0..10).collect::<Vec<i64>>());
    assert_eq!(
        large_rows, small_rows,
        "the first ten rows should not depend on how many follow"
    );

    // 50x the rows; if the read were materializing, time-to-ten would
    // scale with it.
    let ratio = large_time.as_secs_f64() / small_time.as_secs_f64().max(1e-9);
    assert!(
        ratio < 20.0,
        "reading the first ten rows took {ratio:.1}x longer on a 50x larger table \
         ({small_time:?} -> {large_time:?}); that is the shape of a materializing read"
    );
}

/// Requirement 7's second scenario: an abandoned statement releases its
/// cursors, and the connection is immediately usable for a write.
///
/// This is also the test that would hang if `Rows::drop` did not free the
/// worker — the write below goes to the same connection, and the worker is
/// inside the abandoned execution until the channel closes.
#[test]
fn abandoned_statement_releases_cursors() {
    let conn = seeded(1_000);

    let mut rows = conn.query("SELECT a FROM t ORDER BY a").unwrap();
    assert!(rows.next_row().unwrap().is_some());
    assert!(rows.next_row().unwrap().is_some());
    drop(rows);

    // A write on the same connection must proceed, not block behind the
    // cursors the abandoned read had open.
    assert_eq!(conn.execute("DELETE FROM t WHERE a < 10").unwrap(), 10);
    assert_eq!(
        conn.execute("INSERT INTO t VALUES (99999, 'x')").unwrap(),
        1
    );

    // ...and the connection is still fully functional afterwards.
    let remaining: i64 = conn
        .query_row("SELECT count(*) FROM t")
        .unwrap()
        .expect("count returns a row")
        .get(0)
        .unwrap();
    assert_eq!(remaining, 991);
}

/// Repeatedly abandoning streams must not leak the worker into a bad state.
#[test]
fn many_abandoned_streams_leave_the_connection_healthy() {
    let conn = seeded(300);
    for _ in 0..50 {
        let mut rows = conn.query("SELECT a, b FROM t ORDER BY a").unwrap();
        assert!(rows.next_row().unwrap().is_some());
        drop(rows);
    }
    let total: i64 = conn
        .query_row("SELECT count(*) FROM t")
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(total, 300);
}

#[test]
fn values_read_by_name_and_by_index_agree() {
    let conn = seeded(1);
    let row = conn
        .query_row("SELECT a, b FROM t")
        .unwrap()
        .expect("one row");

    assert_eq!(
        row.get::<i64>(0).unwrap(),
        row.get_by_name::<i64>("a").unwrap()
    );
    assert_eq!(
        row.get::<String>(1).unwrap(),
        row.get_by_name::<String>("b").unwrap()
    );
    // Case-insensitive, matching SQLite's column-name comparison.
    assert_eq!(row.get_by_name::<i64>("A").unwrap(), 0);
}

#[test]
fn every_storage_class_reads_back_as_its_rust_type() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE v(i INTEGER, r REAL, t TEXT, b BLOB, n INTEGER)")
        .unwrap();
    conn.execute_with(
        "INSERT INTO v VALUES (?1, ?2, ?3, ?4, ?5)",
        vec![
            Value::from(7i64),
            Value::from(2.5),
            Value::from("hello"),
            Value::from(vec![1u8, 2, 3]),
            Value::Null,
        ],
    )
    .unwrap();

    let row = conn
        .query_row("SELECT i, r, t, b, n FROM v")
        .unwrap()
        .unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 7);
    assert_eq!(row.get::<f64>(1).unwrap(), 2.5);
    assert_eq!(row.get::<String>(2).unwrap(), "hello");
    assert_eq!(row.get::<Vec<u8>>(3).unwrap(), vec![1u8, 2, 3]);
    assert_eq!(row.get::<Option<i64>>(4).unwrap(), None);

    // An INTEGER widens to f64 (what sqlite3_column_double does)...
    assert_eq!(row.get::<f64>(0).unwrap(), 7.0);
    // ...but a REAL does not narrow to i64, because truncating silently is
    // how a rowid becomes wrong.
    assert!(matches!(row.get::<i64>(1), Err(Error::TypeMismatch { .. })));
    // bool follows SQLite: 0 is false, anything else true.
    assert!(row.get::<bool>(0).unwrap());
    // And NULL into a non-Option type is an error, not a default.
    let err = row.get::<i64>(4).expect_err("NULL is not an i64");
    match err {
        Error::TypeMismatch {
            ref column,
            expected,
            found,
        } => {
            assert_eq!(column, "n", "the error should name the column");
            assert_eq!(expected, "i64");
            assert_eq!(found, "NULL");
        }
        other => panic!("expected TypeMismatch, got {other:?}"),
    }
    assert_eq!(err.sqlite_code(), 20, "should report SQLITE_MISMATCH");
}

#[test]
fn reading_past_the_last_column_or_a_missing_name_errors() {
    let conn = seeded(1);
    let row = conn.query_row("SELECT a, b FROM t").unwrap().unwrap();

    assert_eq!(row.len(), 2);
    assert_eq!(
        row.get::<i64>(5).expect_err("out of range"),
        Error::ColumnIndexOutOfRange { index: 5, len: 2 }
    );
    assert_eq!(
        row.get_by_name::<i64>("nope").expect_err("no such column"),
        Error::ColumnNotFound {
            name: "nope".to_string()
        }
    );
}

/// Honest coverage of a known limit rather than a hidden one: a join or a
/// compound reports positional names, so by-name access does not find the
/// base-table names a caller would expect.
#[test]
fn joins_and_compounds_report_positional_column_names() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE a(x INTEGER);
         CREATE TABLE b(y INTEGER);
         INSERT INTO a VALUES (1);
         INSERT INTO b VALUES (2);",
    )
    .unwrap();

    // Each `Rows` is bound to its own scope. Holding one while issuing the
    // next query is the hazard `Rows` documents, and an earlier draft of
    // this very test deadlocked on it — the worker was parked sending the
    // first result's `Done` while this thread asked for a second one.
    {
        let joined = conn
            .query("SELECT a.x, b.y FROM a JOIN b ON a.x < b.y")
            .unwrap();
        assert_eq!(
            joined.column_names(),
            ["column1", "column2"],
            "a join reports positional names — see Rows::column_names"
        );
    }
    {
        let compound = conn.query("SELECT x FROM a UNION SELECT y FROM b").unwrap();
        assert_eq!(compound.column_names(), ["column1"]);
    }

    // By-index access is unaffected, which is why this is a documented
    // limit rather than a blocker.
    let rows = conn
        .query_all("SELECT a.x, b.y FROM a JOIN b ON a.x < b.y")
        .unwrap();
    assert_eq!(rows.len(), 1);
    let first = rows.first().expect("one row");
    assert_eq!(first.get::<i64>(0).unwrap(), 1);
    assert_eq!(first.get::<i64>(1).unwrap(), 2);
}

/// A query is still a statement: the arity and named-parameter rules apply
/// on the read path exactly as on the write path.
#[test]
fn query_enforces_the_same_parameter_rules_as_execute() {
    let conn = seeded(3);

    assert_eq!(
        conn.query_with("SELECT a FROM t WHERE a = ?1", vec![])
            .expect_err("one placeholder, no values"),
        Error::ParamCount {
            expected: 1,
            found: 0
        }
    );
    assert!(matches!(
        conn.query("SELECT a FROM t WHERE a = :id"),
        Err(Error::NamedParameter { .. })
    ));

    let row = conn
        .query_row_with("SELECT a FROM t WHERE a = ?1", vec![Value::from(2)])
        .unwrap()
        .expect("a match");
    assert_eq!(row.get::<i64>(0).unwrap(), 2);
}

#[test]
fn a_query_returning_nothing_is_an_empty_stream_not_an_error() {
    let conn = seeded(3);
    let mut rows = conn.query("SELECT a FROM t WHERE a = 999").unwrap();
    assert!(rows.next_row().unwrap().is_none());
    assert_eq!(
        conn.query_all("SELECT a FROM t WHERE a = 999")
            .unwrap()
            .len(),
        0
    );
    assert!(conn
        .query_row("SELECT a FROM t WHERE a = 999")
        .unwrap()
        .is_none());
}

/// The counterpart to `partial_read_is_bounded`, so the limit it works
/// around is recorded rather than hidden.
///
/// A blocking operator has to consume its whole input before it can emit
/// anything, so streaming cannot make its time-to-first-row flat. This is
/// not a defect in the streaming path and it is not specific to this crate
/// — stock SQLite sorts the same way. A consumer that needs a bounded
/// first-row latency needs an index the sort can walk, not a different API.
///
/// Asserted as a *contrast*, not an absolute threshold: the same table and
/// the same ten rows, scanned versus sorted.
#[test]
fn blocking_plans_are_linear_by_nature() {
    let conn = seeded(4_000);

    let ten = |sql: &str| -> std::time::Duration {
        let start = Instant::now();
        let mut rows = conn.query(sql).unwrap();
        for _ in 0..10 {
            if rows.next_row().unwrap().is_none() {
                break;
            }
        }
        let elapsed = start.elapsed();
        drop(rows);
        elapsed
    };

    // Warm the page cache so this measures the plan, not the first read of
    // the file. The duration is deliberately discarded.
    ten("SELECT a FROM t");

    let scanned = ten("SELECT a FROM t");
    let sorted = ten("SELECT a FROM t ORDER BY b");

    assert!(
        sorted > scanned,
        "a sort should cost more to first row than a scan on the same table \
         (scan {scanned:?}, sort {sorted:?}) — if this ever inverts, the \
         reasoning in partial_read_is_bounded needs revisiting"
    );
}

/// A small result that is never read must not park the worker.
///
/// This is the accident that is easy to have — `let rows = conn.query(..)`
/// and then forget it — and the reason the result channel holds two batches
/// rather than one. With a single slot the worker blocks sending `Done`
/// into a full channel, and the *next* statement on the connection hangs.
///
/// The test would hang rather than fail if that regressed, which is the
/// strongest form the assertion can take here.
#[test]
fn an_unread_small_result_does_not_block_the_next_statement() {
    let conn = seeded(5);

    let unread = conn.query("SELECT a FROM t").unwrap();
    // Deliberately not read and deliberately still alive.
    assert_eq!(unread.column_names(), ["a"]);

    // Must proceed while `unread` is outstanding.
    assert_eq!(conn.execute("INSERT INTO t VALUES (100, 'x')").unwrap(), 1);
    let count: i64 = conn
        .query_row("SELECT count(*) FROM t")
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(count, 6);

    drop(unread);
}

/// The other half of the same contract, stated so the limit is recorded:
/// a result too large to buffer *does* park the worker, and dropping the
/// handle is what releases it.
///
/// Written as a sequence that must complete, not as a timing assertion —
/// the point is that dropping is sufficient, not how long anything took.
#[test]
fn dropping_a_large_unread_result_releases_the_connection() {
    let conn = seeded(1_000);

    let big = conn.query("SELECT a, b FROM t").unwrap();
    // The worker is now parked mid-scan: more rows than the channel holds,
    // and nothing is reading them.
    drop(big);

    // Released, so the connection serves again.
    assert_eq!(conn.execute("DELETE FROM t WHERE a < 5").unwrap(), 5);
}
