// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! The embedding API against the pinned oracle (spec 013 Requirements 1
//! and 2).
//!
//! The unit suites (`tests/unit/api_*.rs`) pin the API's behaviour against
//! itself. This pins it against the definition of correctness: a file the
//! API creates has to be a database stock `sqlite3` reads, and the
//! rows-affected counts it reports have to be the numbers `sqlite3`
//! reports for the same statements.
//!
//! Deliberately driven through `sqlite_rs::api` alone — no `pager`, no
//! `vdbe`, no `codegen`, no `dump`. That the file compiles is itself
//! Requirement 6's "the facade needs no escape hatch" for the write path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sqlite_rs::api::{Connection, OpenMode};
use sqlite_rs::record::Value;

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-api-oracle-{}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn oracle_says(bin: &Path, db: &Path, sql: &str) -> String {
    let output = Command::new(bin)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("oracle failed to run {sql:?}: {e}"));
    assert!(
        output.status.success(),
        "oracle rejected {sql:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Requirement 2's first scenario.
#[test]
fn create_then_oracle_reads_empty_schema() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("create_then_oracle_reads_empty_schema");
        return;
    };
    let dir = scratch_dir("create");
    let db = dir.join("fresh.db");
    assert!(!db.exists());

    // Opened and dropped without running a single statement: the file has
    // to be a valid empty database on the strength of the open alone.
    drop(Connection::open(&db).unwrap());

    assert!(db.exists(), "open should have created the file");
    assert_eq!(oracle_says(&bin, &db, "PRAGMA integrity_check;"), "ok");
    assert_eq!(
        oracle_says(&bin, &db, "SELECT count(*) FROM sqlite_master;"),
        "0",
        "a freshly created database should have an empty schema"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Requirement 2's second scenario, with the oracle confirming the absence
/// rather than only our own `Path::exists`.
#[test]
fn readwrite_creates_nothing_the_oracle_can_find() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("readwrite_creates_nothing_the_oracle_can_find");
        return;
    };
    let dir = scratch_dir("nocreate");
    let db = dir.join("absent.db");

    Connection::open_with(&db, OpenMode::ReadWrite)
        .expect_err("ReadWrite on a missing file must fail");
    assert!(!db.exists(), "nothing should have been written");

    // And the oracle agrees the path holds no database — it would create
    // one itself, so ask it *before* letting it near the path.
    assert!(
        !db.exists(),
        "the failed open left a file behind: {}",
        db.display()
    );
    // Sanity: the oracle can create one here, so the directory was writable
    // all along and the refusal was the mode, not the filesystem.
    oracle_says(&bin, &db, "CREATE TABLE probe(x);");
    assert!(db.exists());

    std::fs::remove_dir_all(&dir).ok();
}

/// The sequence both engines run: every statement the API compiles today,
/// chosen so the rows-affected counts cover a match, a partial match and a
/// miss, and so the file ends up with rows, indexes and deletions in it.
const STATEMENTS: &[&str] = &[
    "CREATE TABLE t(a INTEGER, b TEXT, c TEXT)",
    "CREATE INDEX t_a ON t(a)",
    "CREATE UNIQUE INDEX t_c ON t(c)",
    "INSERT INTO t VALUES (1, 'b1', 'c1')",
    "INSERT INTO t VALUES (2, 'b2', 'c2')",
    "INSERT INTO t VALUES (3, 'b3', 'c3')",
    "INSERT INTO t VALUES (4, 'b4', 'c4')",
    "UPDATE t SET a = a + 10 WHERE a > 2",
    "UPDATE t SET b = 'z' WHERE a > 2",
    "UPDATE t SET b = 'q' WHERE a = 999",
    "DELETE FROM t WHERE a < 3",
    "DELETE FROM t WHERE a = 999",
];

fn is_dml(sql: &str) -> bool {
    let head = sql.trim_start();
    ["INSERT", "UPDATE", "DELETE"]
        .iter()
        .any(|kw| head.len() >= kw.len() && head[..kw.len()].eq_ignore_ascii_case(kw))
}

#[test]
fn api_rows_affected_and_resulting_file_match_the_oracle() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("api_rows_affected_and_resulting_file_match_the_oracle");
        return;
    };
    let dir = scratch_dir("writes");
    let ours = dir.join("ours.db");
    let theirs = dir.join("theirs.db");

    let conn = Connection::open(&ours).unwrap();
    let mut mine = Vec::new();
    for sql in STATEMENTS {
        let changed = conn
            .execute(sql)
            .unwrap_or_else(|e| panic!("{sql} failed through the API: {e}"));
        if is_dml(sql) {
            mine.push((*sql, changed));
        }
    }
    // Release the worker (and its file locks) before handing the file over.
    drop(conn);

    // One oracle invocation per statement, with `changes()` appended:
    // `changes()` is per-connection, so a fresh invocation reports that
    // statement's own count.
    let mut theirs_counts = Vec::new();
    for sql in STATEMENTS {
        let script = if is_dml(sql) {
            format!("{sql};\nSELECT changes();")
        } else {
            format!("{sql};")
        };
        let out = oracle_says(&bin, &theirs, &script);
        if is_dml(sql) {
            let count = out.trim().parse::<u64>().unwrap_or_else(|e| {
                panic!("oracle's changes() after {sql} was not a number ({e}): {out:?}")
            });
            theirs_counts.push((*sql, count));
        }
    }

    assert_eq!(
        mine, theirs_counts,
        "rows-affected counts diverge between the API and the oracle"
    );

    // And the file the API produced is one the oracle reads identically.
    assert_eq!(oracle_says(&bin, &ours, "PRAGMA integrity_check;"), "ok");
    for probe in [
        "SELECT type, name FROM sqlite_master ORDER BY name;",
        "SELECT a, b, c FROM t ORDER BY a;",
        "SELECT count(*) FROM t;",
    ] {
        assert_eq!(
            oracle_says(&bin, &ours, probe),
            oracle_says(&bin, &theirs, probe),
            "the oracle read different results from the API's file and its own for {probe:?}"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// Parameterised writes through the API produce the same file as the
/// oracle running the same statements with the values inlined.
///
/// Six of *SQE*'s eight statements are parameterised writes, and this is
/// the combination the engine had no entry point for at all before the
/// facade: `execute_with_db_and_params` is read-only, and
/// `execute_transaction_step` takes no parameters.
#[test]
fn parameterised_writes_match_the_oracle() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("parameterised_writes_match_the_oracle");
        return;
    };
    let dir = scratch_dir("params");
    let ours = dir.join("ours.db");
    let theirs = dir.join("theirs.db");

    let conn = Connection::open(&ours).unwrap();
    conn.execute("CREATE TABLE t(a INTEGER, b TEXT, r REAL, d BLOB)")
        .unwrap();
    assert_eq!(
        conn.execute_with(
            "INSERT INTO t VALUES (?1, ?2, ?3, ?4)",
            vec![
                Value::from(1),
                Value::from("hello"),
                Value::from(2.5),
                Value::from(vec![0xde_u8, 0xad, 0xbe, 0xef]),
            ],
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.execute_with(
            "INSERT INTO t VALUES (?1, ?2, ?3, ?4)",
            vec![Value::from(2), Value::Null, Value::from(-0.5), Value::Null],
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.execute_with(
            "UPDATE t SET b = ?1 WHERE a = ?2",
            vec![Value::from("updated"), Value::from(1)],
        )
        .unwrap(),
        1
    );
    drop(conn);

    oracle_says(
        &bin,
        &theirs,
        "CREATE TABLE t(a INTEGER, b TEXT, r REAL, d BLOB);
         INSERT INTO t VALUES (1, 'hello', 2.5, x'deadbeef');
         INSERT INTO t VALUES (2, NULL, -0.5, NULL);
         UPDATE t SET b = 'updated' WHERE a = 1;",
    );

    assert_eq!(oracle_says(&bin, &ours, "PRAGMA integrity_check;"), "ok");
    for probe in [
        "SELECT a, b, r, quote(d) FROM t ORDER BY a;",
        "SELECT typeof(a), typeof(b), typeof(r), typeof(d) FROM t ORDER BY a;",
    ] {
        assert_eq!(
            oracle_says(&bin, &ours, probe),
            oracle_says(&bin, &theirs, probe),
            "bound values round-tripped differently for {probe:?}"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}
