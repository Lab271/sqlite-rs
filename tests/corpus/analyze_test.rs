//! `ANALYZE` end-to-end acceptance (#461, spec 011): written via the CLI's
//! `exec` subcommand against a scratch database, verified by reading
//! `sqlite_stat1` back through `query` — same scratch-file-plus-CLI
//! pattern `cli_write_test.rs` uses for the other DDL/DML statements.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-analyze-{label}-{}-{n}",
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

fn exec_ok(db: &Path, sql: &str) {
    let output = run_exec(db, sql);
    assert!(
        output.status.success(),
        "exec {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_query(db: &Path, sql: &str) -> String {
    let output = Command::new(CLI)
        .arg("query")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} query {} {sql:?}: {e}", db.display()));
    assert!(
        output.status.success(),
        "query {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A fresh scratch db with a bootstrap table, created via the pinned
/// oracle (matches `cli_write_test`'s `seed_db`: `sqlite-rs exec` itself
/// can't create a brand-new database file — see `dump::open`, which
/// requires the file to already exist). `None` when no pinned oracle is
/// available — callers should `skip_no_oracle` and return.
fn seed_db(label: &str) -> Option<PathBuf> {
    let oracle = pinned_oracle()?;
    let db = scratch_db(label);
    let status = Command::new(&oracle)
        .arg(&db)
        .arg("CREATE TABLE seed_bootstrap(x)")
        .status()
        .unwrap();
    assert!(status.success());
    Some(db)
}

/// spec 011/Req 1 scenario "Bare ANALYZE populates stats for every table".
#[test]
fn bare_analyze_populates_all_tables() {
    let Some(db) = seed_db("bare-all") else {
        return skip_no_oracle("bare_analyze_populates_all_tables");
    };
    exec_ok(&db, "CREATE TABLE t1(a)");
    exec_ok(&db, "CREATE TABLE t2(a)");
    exec_ok(&db, "INSERT INTO t1 VALUES (1), (2), (3)");
    exec_ok(&db, "INSERT INTO t2 VALUES (1)");

    exec_ok(&db, "ANALYZE");

    let rows = run_query(&db, "SELECT tbl, stat FROM sqlite_stat1 ORDER BY tbl");
    assert!(rows.contains("t1") && rows.contains('3'), "got: {rows}");
    assert!(rows.contains("t2") && rows.contains('1'), "got: {rows}");
}

/// spec 011/Req 1 scenario "ANALYZE table-name scopes to one table".
#[test]
fn analyze_single_table_scopes_stats() {
    let Some(db) = seed_db("scoped") else {
        return skip_no_oracle("analyze_single_table_scopes_stats");
    };
    exec_ok(&db, "CREATE TABLE t1(a)");
    exec_ok(&db, "CREATE TABLE t2(a)");
    exec_ok(&db, "INSERT INTO t1 VALUES (1), (2)");

    exec_ok(&db, "ANALYZE t1");

    let rows = run_query(&db, "SELECT tbl FROM sqlite_stat1");
    assert!(rows.contains("t1"), "got: {rows}");
    assert!(!rows.contains("t2"), "got: {rows}");
}

/// spec 011/Req 1 scenario "ANALYZE of an unknown table reports a clean
/// error".
#[test]
fn analyze_unknown_table_reports_clean_error() {
    let Some(db) = seed_db("unknown") else {
        return skip_no_oracle("analyze_unknown_table_reports_clean_error");
    };
    let output = run_exec(&db, "ANALYZE ghost");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("no such table"),
        "got: {stderr}"
    );
}

/// spec 011/Req 2 scenario "Re-running ANALYZE replaces stale stats".
#[test]
fn re_analyze_replaces_stale_stats() {
    let Some(db) = seed_db("re-run") else {
        return skip_no_oracle("re_analyze_replaces_stale_stats");
    };
    exec_ok(&db, "CREATE TABLE t(a)");
    exec_ok(&db, "INSERT INTO t VALUES (1)");
    exec_ok(&db, "ANALYZE t");

    exec_ok(&db, "INSERT INTO t VALUES (2), (3)");
    exec_ok(&db, "ANALYZE t");

    let rows = run_query(&db, "SELECT tbl, stat FROM sqlite_stat1 WHERE tbl = 't'");
    let row_lines: Vec<&str> = rows.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        row_lines.len(),
        1,
        "expected exactly one t row, got: {rows}"
    );
    assert!(
        rows.contains('3'),
        "expected refreshed count of 3, got: {rows}"
    );
}

/// spec 011/Req 4 scenario "Cost model does not change behavior without
/// stats": a join whose `ON` clause structurally matches a `UNIQUE`
/// index still compiles to a `SEARCH ... USING INDEX` access, exactly
/// as it did before #461, when `ANALYZE` has never run.
#[test]
fn join_access_unchanged_without_analyze() {
    let Some(db) = seed_db("join-no-stats") else {
        return skip_no_oracle("join_access_unchanged_without_analyze");
    };
    exec_ok(&db, "CREATE TABLE t1(a INTEGER)");
    exec_ok(&db, "CREATE TABLE t2(x INTEGER)");
    exec_ok(&db, "CREATE UNIQUE INDEX idx_x ON t2(x)");
    exec_ok(&db, "INSERT INTO t1 VALUES (1), (2), (3)");
    exec_ok(&db, "INSERT INTO t2 VALUES (1), (2), (3)");

    let plan = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM t1 JOIN t2 ON t2.x = t1.a",
    );
    assert!(plan.contains("SEARCH t2 USING INDEX idx_x"), "got: {plan}");
}

/// spec 011/Req 4 scenario "Cost model can veto a unique-index seek
/// stats show is not worth it": once `ANALYZE` stats for `idx_x` are
/// skewed so the cost model estimates the index probe as more
/// expensive than a full scan, the join falls back to `SCAN` instead
/// of `SEARCH ... USING INDEX`.
#[test]
fn cost_model_can_veto_expensive_index_seek() {
    let Some(db) = seed_db("join-veto") else {
        return skip_no_oracle("cost_model_can_veto_expensive_index_seek");
    };
    exec_ok(&db, "CREATE TABLE t1(a INTEGER)");
    exec_ok(&db, "CREATE TABLE t2(x INTEGER)");
    exec_ok(&db, "CREATE UNIQUE INDEX idx_x ON t2(x)");
    exec_ok(&db, "INSERT INTO t1 VALUES (1), (2), (3)");
    exec_ok(&db, "INSERT INTO t2 VALUES (1), (2), (3)");
    exec_ok(&db, "ANALYZE");

    let plan_before = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM t1 JOIN t2 ON t2.x = t1.a",
    );
    assert!(
        plan_before.contains("SEARCH t2 USING INDEX idx_x"),
        "got: {plan_before}"
    );

    // Skew idx_x's avg_eq far above t2's own row count -- a
    // pathological "this index is not selective" case ANALYZE could
    // in principle record for real (e.g. mostly-duplicate data), here
    // just hand-written to exercise the veto deterministically.
    exec_ok(
        &db,
        "UPDATE sqlite_stat1 SET stat = '3 50000' WHERE tbl = 't2' AND idx = 'idx_x'",
    );

    let plan_after = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM t1 JOIN t2 ON t2.x = t1.a",
    );
    assert!(plan_after.contains("SCAN t2"), "got: {plan_after}");
    assert!(
        !plan_after.contains("USING INDEX idx_x"),
        "got: {plan_after}"
    );
}
