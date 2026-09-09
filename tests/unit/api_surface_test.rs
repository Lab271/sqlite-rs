// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! The facade is sufficient on its own (spec 013 Requirement 6).
//!
//! Today `src/lib.rs` exports the whole engine — `btree`, `codegen`,
//! `dump`, `pager`, `parser`, `planner`, `vdbe`, `vfs` — while
//! `CHANGELOG.md` says pre-1.0 minor bumps may break the public API. A
//! consumer wiring `dump::open` to `execute_transaction_step` is therefore
//! building on items carrying no promise, and *SQE* confines every
//! `sqlite_rs::` reference to one module precisely because of that. The
//! stability policy is what fixes it; this test is what stops the policy
//! from being a lie.
//!
//! Two things are asserted, and the second is the one with teeth.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// The *only* import in this file. Nothing from `pager`, `vdbe`, `codegen`,
// `dump`, `btree`, `parser`, `planner`, `schema`, `header` or `vfs`.
use sqlite_rs::api::{
    Connection, Error, FromValue, OpenMode, Row, Rows, Statement, TransactionBehavior, Value,
};

/// Requirement 6's first scenario: the whole workload, using only the items
/// this spec defines.
///
/// That it compiles is the assertion. Every name it uses comes from
/// `sqlite_rs::api`, so if the facade were missing a capability this
/// function could not be written — which is what "no escape hatch" means
/// operationally.
#[test]
fn facade_is_sufficient_alone() {
    // Create a database that did not exist.
    let conn = Connection::open_in_memory().unwrap();

    // Define a schema.
    conn.execute_batch(
        "CREATE TABLE iceberg_tables(
             catalog_name TEXT,
             table_namespace TEXT,
             table_name TEXT,
             metadata_location TEXT
         );
         CREATE UNIQUE INDEX iceberg_tables_pk
             ON iceberg_tables(catalog_name, table_namespace, table_name);",
    )
    .unwrap();

    // Prepare and bind.
    let insert = conn
        .prepare(
            "INSERT INTO iceberg_tables
                 (catalog_name, table_namespace, table_name, metadata_location)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .unwrap();
    assert_eq!(insert.param_count(), 4);

    // Run a transaction, and read the rows-affected count.
    let tx = conn
        .transaction_with(TransactionBehavior::Immediate)
        .unwrap();
    for (ns, name) in [("prod", "orders"), ("prod", "customers"), ("dev", "orders")] {
        let changed = insert
            .execute(vec![
                Value::from("main"),
                Value::from(ns),
                Value::from(name),
                Value::from(format!("s3://bucket/{ns}/{name}/v1.json")),
            ])
            .unwrap();
        assert_eq!(changed, 1);
    }
    tx.commit().unwrap();
    assert_eq!(conn.changes().unwrap(), 1);

    // Read rows back as typed values, by index and by name.
    let lookup = conn
        .prepare(
            "SELECT metadata_location FROM iceberg_tables
             WHERE catalog_name = ?1 AND table_namespace = ?2 AND table_name = ?3",
        )
        .unwrap();
    let row: Row = lookup
        .query_row(vec![
            Value::from("main"),
            Value::from("prod"),
            Value::from("orders"),
        ])
        .unwrap()
        .expect("the row was just written");
    let location: String = row.get(0).unwrap();
    assert_eq!(location, "s3://bucket/prod/orders/v1.json");
    assert_eq!(
        row.get_by_name::<String>("metadata_location").unwrap(),
        location
    );

    // Stream a multi-row result.
    let mut rows: Rows = conn
        .query("SELECT table_namespace, table_name FROM iceberg_tables")
        .unwrap();
    let mut seen = 0;
    while let Some(row) = rows.next_row().unwrap() {
        let _ns: String = row.get(0).unwrap();
        seen += 1;
    }
    assert_eq!(seen, 3);
    drop(rows);

    // The optimistic-concurrency swap, which is what the count is for.
    let swap = conn
        .prepare(
            "UPDATE iceberg_tables SET metadata_location = ?1
             WHERE table_name = ?2 AND metadata_location = ?3",
        )
        .unwrap();
    assert_eq!(
        swap.execute(vec![
            Value::from("s3://bucket/prod/orders/v2.json"),
            Value::from("orders"),
            Value::from("s3://bucket/prod/orders/v1.json"),
        ])
        .unwrap(),
        1,
        "the first swap should win"
    );
    assert_eq!(
        swap.execute(vec![
            Value::from("s3://bucket/prod/orders/v3.json"),
            Value::from("orders"),
            Value::from("s3://bucket/prod/orders/v1.json"),
        ])
        .unwrap(),
        0,
        "the second swap should lose the race, not overwrite"
    );

    // Delete, and inspect an error's SQLite result code.
    assert_eq!(
        conn.execute("DELETE FROM iceberg_tables WHERE table_namespace = 'dev'")
            .unwrap(),
        1
    );
    let dup = conn
        .execute(
            "INSERT INTO iceberg_tables
                 (catalog_name, table_namespace, table_name, metadata_location)
             VALUES ('main', 'prod', 'orders', 'x')",
        )
        .expect_err("violates the unique index");
    assert_eq!(dup.sqlite_code(), 19, "SQLITE_CONSTRAINT");
    assert!(!dup.is_retryable());

    // And the read-only mode and the busy timeout are configurable from
    // here too, completing the surface the spec lists.
    conn.set_busy_timeout(std::time::Duration::from_millis(100))
        .unwrap();
    let _ = OpenMode::ReadOnly;
    let _: fn(&Value) -> Result<i64, &'static str> = <i64 as FromValue>::from_value;
    let _: fn(&Statement) -> usize = Statement::param_count;
    let _: fn(&Error) -> bool = Error::is_retryable;
}

/// The assertion with teeth: this file must not name the engine.
///
/// `facade_is_sufficient_alone` above proves the facade is *enough* only so
/// long as nobody quietly widens it. If a future capability gap were
/// "fixed" by importing `sqlite_rs::pager`, that test would keep passing
/// and Requirement 6 would be silently false. Reading our own source is the
/// only way to catch it, and `tests/unit/layer_isolation.rs` already uses
/// this idiom for the engine's internal layering.
#[test]
fn this_file_names_no_engine_module() {
    let source = include_str!("api_surface_test.rs");

    // Comment lines are stripped before scanning. The prose here and above
    // names engine modules deliberately — to say what must not appear —
    // and matching on that would make the test fail on its own explanation.
    // A real `use sqlite_rs::pager` is never inside a comment.
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    // Every module `src/lib.rs` declares apart from `api`.
    const ENGINE: &[&str] = &[
        "btree",
        "codegen",
        "dump",
        "format",
        "header",
        "integrity",
        "pager",
        "parser",
        "planner",
        "record",
        "schema",
        "sys",
        "vdbe",
        "vfs",
    ];

    for module in ENGINE {
        let needle = format!("sqlite_rs::{module}");
        assert!(
            !code.contains(&needle),
            "{needle} appears in this file — either the facade has a gap that was \
             worked around by reaching into the engine, or this list is stale. \
             Requirement 6 says a consumer should never need to."
        );
    }
}
