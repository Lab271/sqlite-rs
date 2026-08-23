//! End-to-end oracle-diff tests for compound `SELECT` — `UNION ALL`
//! (#240) and plain `UNION` (#377/#378) — via the `sqlite-rs` CLI's
//! `exec`/`query` subcommands, mirroring `join_test.rs`'s
//! scratch-db-per-test shape.

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-union-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
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

/// A fresh scratch db seeded with two same-shape tables, `t1`/`t2`.
fn union_fixture_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    let ddls = [
        "CREATE TABLE t1(a INTEGER, name TEXT)",
        "CREATE TABLE t2(b INTEGER, tag TEXT)",
    ];
    let rows = [
        "INSERT INTO t1 VALUES (1, 'alice'), (2, 'bob')",
        "INSERT INTO t2 VALUES (3, 'x'), (4, 'y')",
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

fn assert_matches_oracle(db: &Path, sql: &str, test_name: &str) {
    let ours = run_query_ok(db, sql);
    if let Some(oracle) = pinned_oracle() {
        let theirs = oracle_select(&oracle, db, sql);
        assert_eq!(ours, theirs, "mismatch for {sql:?}");
    } else {
        skip_no_oracle(test_name);
    }
}

#[test]
fn union_all_concatenates_without_deduplication() {
    let db = union_fixture_db("basic");
    let output = run_query_ok(&db, "SELECT a FROM t1 UNION ALL SELECT b FROM t2");
    assert_eq!(output, "1\n2\n3\n4\n");
    assert_matches_oracle(
        &db,
        "SELECT a FROM t1 UNION ALL SELECT b FROM t2",
        "union_all_concatenates_without_deduplication",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// Duplicate rows across the two arms must both survive — this is the
/// "no dedup" half of the acceptance criteria (dedup is UNION, deferred
/// to Phase 2).
#[test]
fn union_all_keeps_duplicate_rows() {
    let db = union_fixture_db("dupes");
    let output = run_query_ok(&db, "SELECT a FROM t1 UNION ALL SELECT a FROM t1");
    assert_eq!(output, "1\n2\n1\n2\n");
    assert_matches_oracle(
        &db,
        "SELECT a FROM t1 UNION ALL SELECT a FROM t1",
        "union_all_keeps_duplicate_rows",
    );
}

/// A mismatched arm column count is rejected at compile time, not
/// silently padded/truncated.
#[test]
fn column_count_mismatch_is_rejected() {
    let db = union_fixture_db("mismatch");
    let output = run_query(&db, "SELECT a FROM t1 UNION ALL SELECT a, b FROM t2");
    assert!(
        !output.status.success(),
        "expected column-count mismatch to fail, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("same number of result columns"),
        "expected a column-count-mismatch error, got: {stderr}"
    );
}

/// #268: SQLite performs no coercion between `UNION ALL` arms — a
/// column position with `INTEGER` affinity in one arm and `TEXT` in
/// another keeps each arm's own storage class/affinity untouched (this
/// is SQLite's dynamic typing: `UNION ALL` is pure row concatenation,
/// not a typed-column operation).
#[test]
fn union_all_does_not_coerce_between_mismatched_arm_types() {
    let db = scratch_db("type_mismatch");
    let ddls = ["CREATE TABLE ti(a INTEGER)", "CREATE TABLE ts(a TEXT)"];
    let rows = [
        "INSERT INTO ti VALUES (1), (2)",
        "INSERT INTO ts VALUES ('x'), ('y')",
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
    let output = run_query_ok(&db, "SELECT a FROM ti UNION ALL SELECT a FROM ts");
    assert_eq!(output, "1\n2\nx\ny\n");
    assert_matches_oracle(
        &db,
        "SELECT a FROM ti UNION ALL SELECT a FROM ts",
        "union_all_does_not_coerce_between_mismatched_arm_types",
    );
}

/// Multiple `UNION ALL` arms chain: `A UNION ALL B UNION ALL C`.
#[test]
fn multiple_union_all_arms_chain() {
    let db = union_fixture_db("chain");
    let output = run_query_ok(
        &db,
        "SELECT a FROM t1 UNION ALL SELECT b FROM t2 UNION ALL SELECT a FROM t1",
    );
    assert_eq!(output, "1\n2\n3\n4\n1\n2\n");
    assert_matches_oracle(
        &db,
        "SELECT a FROM t1 UNION ALL SELECT b FROM t2 UNION ALL SELECT a FROM t1",
        "multiple_union_all_arms_chain",
    );
}

/// A `WHERE` clause on an individual arm filters just that arm, not
/// the whole compound result.
#[test]
fn where_clause_filters_only_its_own_arm() {
    let db = union_fixture_db("where_per_arm");
    assert_matches_oracle(
        &db,
        "SELECT a FROM t1 WHERE a = 1 UNION ALL SELECT b FROM t2",
        "where_clause_filters_only_its_own_arm",
    );
}

/// Plain `UNION` (#377/#378) deduplicates — a row that appears in both
/// arms is emitted only once, unlike `UNION ALL`.
#[test]
fn union_dedups_duplicate_rows() {
    let db = union_fixture_db("union_dupes");
    let output = run_query_ok(&db, "SELECT a FROM t1 UNION SELECT a FROM t1");
    assert_eq!(output, "1\n2\n");
    assert_matches_oracle(
        &db,
        "SELECT a FROM t1 UNION SELECT a FROM t1",
        "union_dedups_duplicate_rows",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// Basic `UNION` with no overlapping rows between arms — every row
/// from both arms survives, same as `UNION ALL` would here.
#[test]
fn union_basic_no_duplicates() {
    let db = union_fixture_db("union_basic");
    let output = run_query_ok(&db, "SELECT a FROM t1 UNION SELECT b FROM t2");
    assert_eq!(output, "1\n2\n3\n4\n");
    assert_matches_oracle(
        &db,
        "SELECT a FROM t1 UNION SELECT b FROM t2",
        "union_basic_no_duplicates",
    );
}

/// A mismatched arm column count is rejected at compile time for plain
/// `UNION` too, same as `UNION ALL`.
#[test]
fn union_column_count_mismatch_is_rejected() {
    let db = union_fixture_db("union_mismatch");
    let output = run_query(&db, "SELECT a FROM t1 UNION SELECT a, b FROM t2");
    assert!(
        !output.status.success(),
        "expected column-count mismatch to fail, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("same number of result columns"),
        "expected a column-count-mismatch error, got: {stderr}"
    );
}

/// `INTERSECT`/`EXCEPT` remain unsupported (deferred to V7).
#[test]
fn intersect_is_rejected_as_unsupported() {
    let db = union_fixture_db("intersect_unsupported");
    let output = run_query(&db, "SELECT a FROM t1 INTERSECT SELECT a FROM t1");
    assert!(!output.status.success());
}
