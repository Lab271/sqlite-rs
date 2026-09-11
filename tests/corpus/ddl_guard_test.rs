// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #697 acceptance: `CREATE ... IF NOT EXISTS` / `DROP ... IF EXISTS`
//! guards were parsed but never consulted before emission — a second
//! `CREATE TABLE IF NOT EXISTS` against an existing table appended a
//! second `sqlite_master` row and leaked its root page, corrupting the
//! file (`PRAGMA integrity_check` then reports `Page N: never used` and,
//! for a composite key, `wrong # of entries in index sqlite_autoindex_*`
//! on top). `CREATE TABLE IF NOT EXISTS` is the idiom SQE's catalog
//! bootstrap runs on every startup, so this corrupted a self-created
//! database on its second run.
//!
//! Covers all three `CREATE ... IF NOT EXISTS` forms, both `DROP ...
//! IF EXISTS` forms, and the mirror claim that dropping/creating
//! *without* the guard still behaves like stock SQLite (fails on a
//! duplicate create, fails on a missing drop).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-ddl-guard-{label}-{}-{n}",
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

fn oracle_scalar(oracle: &Path, db: &Path, sql: &str) -> String {
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// `CREATE TABLE IF NOT EXISTS` run twice: one `sqlite_master` row, no
/// error, no leaked page, prior data intact, and the oracle still
/// considers the file sound. The issue's headline composite-PK repro.
#[test]
fn create_table_if_not_exists_twice_is_a_clean_no_op() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("ddl_guard");
        return;
    };
    let db = scratch_db("create-table-twice");
    let ddl = "CREATE TABLE IF NOT EXISTS t (a TEXT, b TEXT, PRIMARY KEY (a, b))";
    exec_ok(&db, ddl);
    exec_ok(&db, "INSERT INTO t VALUES ('x', 'y')");
    let page_count_before = oracle_scalar(&oracle, &db, "PRAGMA page_count");

    exec_ok(&db, ddl);

    let table_rows = oracle_scalar(
        &oracle,
        &db,
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 't'",
    );
    assert_eq!(table_rows, "1", "expected exactly one sqlite_master row");
    let page_count_after = oracle_scalar(&oracle, &db, "PRAGMA page_count");
    assert_eq!(
        page_count_before, page_count_after,
        "second no-op create must not leak a page"
    );
    assert_integrity_check_ok(&oracle, &db);
    let row_count = oracle_scalar(&oracle, &db, "SELECT count(*) FROM t");
    assert_eq!(row_count, "1", "data between the two runs must survive");
}

/// The single-column, no-constraint shape from the issue's isolation
/// table: same clean no-op, no composite key involved.
#[test]
fn create_table_if_not_exists_twice_without_a_constraint_is_a_clean_no_op() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("ddl_guard");
        return;
    };
    let db = scratch_db("create-table-twice-simple");
    let ddl = "CREATE TABLE IF NOT EXISTS n (a TEXT)";
    exec_ok(&db, ddl);
    exec_ok(&db, ddl);
    let table_rows = oracle_scalar(
        &oracle,
        &db,
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'n'",
    );
    assert_eq!(table_rows, "1");
    assert_integrity_check_ok(&oracle, &db);
}

/// Without the guard, a duplicate `CREATE TABLE` must still fail, with
/// the oracle's own wording.
#[test]
fn create_table_without_guard_still_fails_on_a_duplicate() {
    let db = scratch_db("create-table-no-guard");
    exec_ok(&db, "CREATE TABLE t (a TEXT)");
    let output = run_exec(&db, "CREATE TABLE t (a TEXT)");
    assert!(
        !output.status.success(),
        "expected a duplicate CREATE TABLE to fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("table t already exists"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `CREATE INDEX IF NOT EXISTS` run twice is a clean no-op; without the
/// guard it fails on the duplicate.
#[test]
fn create_index_if_not_exists_twice_is_a_clean_no_op() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("ddl_guard");
        return;
    };
    let db = scratch_db("create-index-twice");
    exec_ok(&db, "CREATE TABLE t (a TEXT)");
    let ddl = "CREATE INDEX IF NOT EXISTS i ON t (a)";
    exec_ok(&db, ddl);
    exec_ok(&db, ddl);
    let index_rows = oracle_scalar(
        &oracle,
        &db,
        "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'i'",
    );
    assert_eq!(index_rows, "1");
    assert_integrity_check_ok(&oracle, &db);

    let output = run_exec(&db, "CREATE INDEX i ON t (a)");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("index i already exists"));
}

/// `CREATE VIEW IF NOT EXISTS` run twice is a clean no-op; without the
/// guard it fails on the duplicate.
#[test]
fn create_view_if_not_exists_twice_is_a_clean_no_op() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("ddl_guard");
        return;
    };
    let db = scratch_db("create-view-twice");
    exec_ok(&db, "CREATE TABLE t (a TEXT)");
    let ddl = "CREATE VIEW IF NOT EXISTS v AS SELECT * FROM t";
    exec_ok(&db, ddl);
    exec_ok(&db, ddl);
    let view_rows = oracle_scalar(
        &oracle,
        &db,
        "SELECT count(*) FROM sqlite_master WHERE type = 'view' AND name = 'v'",
    );
    assert_eq!(view_rows, "1");
    assert_integrity_check_ok(&oracle, &db);

    let output = run_exec(&db, "CREATE VIEW v AS SELECT * FROM t");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("view v already exists"));
}

/// Followup to #697: tables and views share one namespace, and the
/// already-exists error must name the kind of the *existing* object, not
/// the kind of the statement. Oracle-measured (3.53.4):
/// `CREATE TABLE v(a)` against an existing view `v` reports
/// "view v already exists"; `CREATE VIEW t AS ...` against an existing
/// table `t` reports "table t already exists".
#[test]
fn cross_kind_table_view_clash_without_guard_names_the_existing_kind() {
    let db = scratch_db("cross-kind-no-guard");
    exec_ok(&db, "CREATE TABLE t (a)");
    exec_ok(&db, "CREATE VIEW v AS SELECT a FROM t");

    let output = run_exec(&db, "CREATE TABLE v(a)");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("view v already exists"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = run_exec(&db, "CREATE VIEW t AS SELECT 1");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("table t already exists"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The table/view namespace clash above is the same namespace `IF NOT
/// EXISTS` guards, so (oracle-measured) the guard suppresses it in both
/// directions: a clean no-op, schema untouched.
#[test]
fn cross_kind_table_view_clash_with_guard_is_a_clean_no_op() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("ddl_guard");
        return;
    };
    let db = scratch_db("cross-kind-guard");
    exec_ok(&db, "CREATE TABLE t (a)");
    exec_ok(&db, "CREATE VIEW v AS SELECT a FROM t");

    exec_ok(&db, "CREATE TABLE IF NOT EXISTS v(a)");
    let view_rows = oracle_scalar(
        &oracle,
        &db,
        "SELECT count(*) FROM sqlite_master WHERE name = 'v'",
    );
    assert_eq!(
        view_rows, "1",
        "guarded cross-kind create must not add a row"
    );
    let view_type = oracle_scalar(
        &oracle,
        &db,
        "SELECT type FROM sqlite_master WHERE name = 'v'",
    );
    assert_eq!(view_type, "view", "v must still be a view, not a table");

    exec_ok(&db, "CREATE VIEW IF NOT EXISTS t AS SELECT 1");
    let table_rows = oracle_scalar(
        &oracle,
        &db,
        "SELECT count(*) FROM sqlite_master WHERE name = 't'",
    );
    assert_eq!(
        table_rows, "1",
        "guarded cross-kind create must not add a row"
    );
    let table_type = oracle_scalar(
        &oracle,
        &db,
        "SELECT type FROM sqlite_master WHERE name = 't'",
    );
    assert_eq!(table_type, "table", "t must still be a table, not a view");
    assert_integrity_check_ok(&oracle, &db);
}

/// `CREATE INDEX` naming an existing table or view is a different
/// namespace clash than index-vs-index — oracle-measured wording is
/// "there is already a table named X" in both directions (even when the
/// clash is with a view, not a table).
#[test]
fn create_index_name_clash_with_table_or_view_without_guard() {
    let db = scratch_db("index-vs-table-no-guard");
    exec_ok(&db, "CREATE TABLE t (a)");
    exec_ok(&db, "CREATE VIEW v AS SELECT a FROM t");

    let output = run_exec(&db, "CREATE INDEX t ON t(a)");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("there is already a table named t"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = run_exec(&db, "CREATE INDEX v ON t(a)");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("there is already a table named v"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The index-vs-table/view clash is a different namespace than the one
/// `IF NOT EXISTS` guards on `CREATE INDEX`, so (oracle-measured) the
/// guard does NOT suppress it — still an error.
#[test]
fn create_index_if_not_exists_does_not_suppress_table_or_view_name_clash() {
    let db = scratch_db("index-vs-table-guard");
    exec_ok(&db, "CREATE TABLE t (a)");
    exec_ok(&db, "CREATE VIEW v AS SELECT a FROM t");

    let output = run_exec(&db, "CREATE INDEX IF NOT EXISTS t ON t(a)");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("there is already a table named t"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = run_exec(&db, "CREATE INDEX IF NOT EXISTS v ON t(a)");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("there is already a table named v"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `CREATE TABLE`/`CREATE VIEW` naming an existing index is a different
/// namespace clash — oracle-measured wording is "there is already an
/// index named X".
#[test]
fn create_table_or_view_name_clash_with_index_without_guard() {
    let db = scratch_db("table-vs-index-no-guard");
    exec_ok(&db, "CREATE TABLE t (a)");
    exec_ok(&db, "CREATE INDEX ix ON t(a)");

    let output = run_exec(&db, "CREATE TABLE ix(a)");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("there is already an index named ix"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = run_exec(&db, "CREATE VIEW ix AS SELECT 1");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("there is already an index named ix"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The table/view-vs-index clash is a different namespace than the one
/// `IF NOT EXISTS` guards on `CREATE TABLE`/`CREATE VIEW`, so
/// (oracle-measured) the guard does NOT suppress it — still an error.
#[test]
fn create_table_or_view_if_not_exists_does_not_suppress_index_name_clash() {
    let db = scratch_db("table-vs-index-guard");
    exec_ok(&db, "CREATE TABLE t (a)");
    exec_ok(&db, "CREATE INDEX ix ON t(a)");

    let output = run_exec(&db, "CREATE TABLE IF NOT EXISTS ix(a)");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("there is already an index named ix"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = run_exec(&db, "CREATE VIEW IF NOT EXISTS ix AS SELECT 1");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("there is already an index named ix"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `DROP TABLE IF EXISTS` on a table that was never there is a clean
/// no-op (rc 0); without the guard it still fails as before.
#[test]
fn drop_table_if_exists_on_a_missing_table_is_a_clean_no_op() {
    let db = scratch_db("drop-table-missing");
    exec_ok(&db, "DROP TABLE IF EXISTS nope");

    let output = run_exec(&db, "DROP TABLE nope");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no such table"));
}

/// `DROP INDEX IF EXISTS` on an index that was never there is a clean
/// no-op (rc 0); without the guard it still fails as before.
#[test]
fn drop_index_if_exists_on_a_missing_index_is_a_clean_no_op() {
    let db = scratch_db("drop-index-missing");
    exec_ok(&db, "DROP INDEX IF EXISTS nope");

    let output = run_exec(&db, "DROP INDEX nope");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no such index"));
}
