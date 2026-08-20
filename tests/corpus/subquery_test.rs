//! End-to-end oracle-diff tests for non-correlated subquery expressions
//! (#238): scalar `(SELECT ...)`, `IN (SELECT ...)`/`NOT IN
//! (SELECT ...)`, and `EXISTS (SELECT ...)`/`NOT EXISTS (SELECT ...)`
//! — via the `sqlite-rs` CLI's `exec`/`query` subcommands, mirroring
//! `join_test.rs`'s scratch-db-per-test shape.

use crate::oracle::{pinned_oracle, skip_no_oracle};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-subquery-{label}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

fn run_exec(db: &Path, sql: &str) -> Output {
    Command::new(CLI)
        .arg("exec")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} exec {} {sql:?}: {e}", db.display()))
}

fn run_query(db: &Path, sql: &str) -> Output {
    Command::new(CLI)
        .arg("query")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} query {} {sql:?}: {e}", db.display()))
}

fn run_query_ok(db: &Path, sql: &str) -> String {
    let output = run_query(db, sql);
    assert!(
        output.status.success(),
        "query {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn oracle_select(oracle: &Path, db: &Path, sql: &str) -> String {
    let output = Command::new(oracle)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running oracle on {}: {e}", db.display()));
    assert!(
        output.status.success(),
        "oracle query {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_matches_oracle(db: &Path, sql: &str, test_name: &str) {
    let ours = run_query_ok(db, sql);
    if let Some(oracle) = pinned_oracle() {
        let theirs = oracle_select(&oracle, db, sql);
        assert_eq!(ours, theirs, "mismatch for {sql:?}");
    } else {
        skip_no_oracle(test_name);
    }
}

/// A fresh scratch db with a `t` (main) and `other` (subquery target)
/// table — same oracle-if-available, else-bootstrap-via-`exec` shape as
/// `join_test.rs`'s `join_fixture_db`.
fn subquery_fixture_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    let ddls = [
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
        "CREATE TABLE other(id INTEGER PRIMARY KEY, a_id INTEGER)",
    ];
    let rows = [
        "INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)",
        "INSERT INTO other VALUES (100, 1), (101, 2)",
    ];
    if let Some(oracle) = pinned_oracle() {
        for stmt in ddls.iter().chain(rows.iter()) {
            let status = Command::new(&oracle).arg(&db).arg(stmt).status().unwrap();
            assert!(status.success(), "oracle setup failed: {stmt}");
        }
    } else {
        assert!(run_exec(&db, "CREATE TABLE seed_bootstrap(x)")
            .status
            .success());
        for stmt in ddls.iter().chain(rows.iter()) {
            let output = run_exec(&db, stmt);
            assert!(
                output.status.success(),
                "setup {stmt:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    db
}

// Note: this codebase does not implement SQL aggregates at all yet
// (`compile_value`'s `is_aggregate_call` guard rejects `max`/`min`/
// `count`/etc. everywhere, not just inside a subquery — a pre-existing,
// separate gap), so these tests use a non-aggregate scalar subquery
// rather than the issue's illustrative `max(x)` example.
#[test]
fn scalar_subquery_matches_oracle() {
    let db = subquery_fixture_db("scalar");
    assert_matches_oracle(
        &db,
        "SELECT (SELECT x FROM t WHERE id = 1) FROM t LIMIT 1",
        "scalar_subquery_matches_oracle",
    );
}

/// A scalar subquery with zero matching rows answers NULL.
#[test]
fn scalar_subquery_with_no_rows_is_null() {
    let db = subquery_fixture_db("scalar_empty");
    let output = run_query_ok(&db, "SELECT (SELECT x FROM t WHERE x = 999) FROM t LIMIT 1");
    assert_eq!(output, "\n");
    assert_matches_oracle(
        &db,
        "SELECT (SELECT x FROM t WHERE x = 999) FROM t LIMIT 1",
        "scalar_subquery_with_no_rows_is_null",
    );
}

#[test]
fn scalar_subquery_in_where_clause() {
    let db = subquery_fixture_db("scalar_where");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE x = (SELECT x FROM t WHERE id = 2)",
        "scalar_subquery_in_where_clause",
    );
}

#[test]
fn in_subquery_matches_oracle() {
    let db = subquery_fixture_db("in_subquery");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE id IN (SELECT a_id FROM other)",
        "in_subquery_matches_oracle",
    );
}

#[test]
fn not_in_subquery_matches_oracle() {
    let db = subquery_fixture_db("not_in_subquery");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE id NOT IN (SELECT a_id FROM other)",
        "not_in_subquery_matches_oracle",
    );
}

#[test]
fn exists_subquery_matches_oracle() {
    let db = subquery_fixture_db("exists_subquery");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE EXISTS (SELECT 1 FROM other WHERE other.a_id = 1)",
        "exists_subquery_matches_oracle",
    );
}

#[test]
fn not_exists_subquery_matches_oracle() {
    let db = subquery_fixture_db("not_exists_subquery");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE NOT EXISTS (SELECT 1 FROM other WHERE other.a_id = 1)",
        "not_exists_subquery_matches_oracle",
    );
}

/// A correlated subquery (referencing the enclosing query's `t.id`
/// from inside the subquery's own WHERE clause) must fail cleanly with
/// this pass's documented "correlated subqueries are not yet
/// supported" diagnostic — not panic, not silently mis-compile, and not
/// a generic parse error (it parses fine; the rejection is a codegen
/// decision made once schema information is available). #238 does NOT
/// implement correlated subqueries — this is the issue's "Correlated
/// subqueries work" acceptance criterion, explicitly deferred.
#[test]
fn correlated_subquery_fails_cleanly_not_silently() {
    let db = subquery_fixture_db("correlated");
    let output = run_query(
        &db,
        "SELECT id FROM t WHERE EXISTS (SELECT 1 FROM other WHERE other.a_id = t.id)",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "expected the correlated subquery to fail to compile"
    );
    assert!(
        stderr.contains("correlated subqueries are not yet supported"),
        "expected the documented correlated-subquery diagnostic; got: {stderr}"
    );
    assert!(!stderr.contains("panicked at"), "must not panic: {stderr}");
}

/// `ANY`/`ALL`/`SOME` quantified comparisons and subqueries in `FROM`
/// stay out of scope — must fail cleanly as unsupported/invalid, not
/// panic.
#[test]
fn still_unsupported_subquery_forms_fail_cleanly() {
    let db = subquery_fixture_db("still_unsupported");
    for sql in [
        "SELECT id FROM t WHERE x > ANY (SELECT x FROM t)",
        "SELECT id FROM t WHERE x > ALL (SELECT x FROM t)",
        "SELECT * FROM (SELECT * FROM t) AS sub",
    ] {
        let output = run_query(&db, sql);
        assert!(!output.status.success(), "expected {sql:?} to fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("panicked at"), "must not panic: {stderr}");
    }
}
