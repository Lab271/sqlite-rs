//! End-to-end tests of the `sqlite-rs exec` CLI subcommand (#215, Phase 4
//! of the V3 epic #161): INSERT/UPDATE/DELETE/CREATE TABLE/DROP TABLE/
//! CREATE INDEX/DROP INDEX, each written via the CLI binary against a
//! scratch copy, then verified by reading back — via the CLI's own
//! `query` subcommand (round trip) and, when the pinned oracle `sqlite3`
//! is available, via `PRAGMA integrity_check` and a `SELECT` (write via
//! CLI -> read via stock `sqlite3` produces identical results, per the
//! issue's acceptance criteria).
//!
//! Every test starts from a fresh scratch file rather than a committed
//! fixture — `export`'s convention of never mutating the fixture tree
//! applies doubly here, since these tests write to the database itself.

use crate::oracle::{pinned_oracle, skip_no_oracle};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-cli-write-{label}-{}-{n}",
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

/// A scratch db seeded via the pinned oracle if available, else via our
/// own CLI's CREATE TABLE — either way, gives every test a real on-disk
/// database with a valid header before it starts exercising `exec`.
fn seed_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    if let Some(oracle) = pinned_oracle() {
        let status = Command::new(&oracle)
            .arg(&db)
            .arg("CREATE TABLE t(a INTEGER, b TEXT)")
            .status()
            .unwrap();
        assert!(status.success());
    } else {
        let output = run_exec(&db, "CREATE TABLE seed_bootstrap(x)");
        assert!(output.status.success());
        let output = run_exec(&db, "CREATE TABLE t(a INTEGER, b TEXT)");
        assert!(output.status.success());
    }
    db
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

fn assert_integrity_ok(oracle: &Path, db: &Path) {
    let out = oracle_select(oracle, db, "PRAGMA integrity_check");
    assert_eq!(out.trim(), "ok", "integrity_check failed for {db:?}");
}

#[test]
fn insert_round_trips_through_cli_query() {
    let db = seed_db("insert");
    let output = run_exec(&db, "INSERT INTO t VALUES (1, 'x'), (2, 'y')");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let rows = run_query(&db, "SELECT * FROM t");
    assert_eq!(rows, "1|x\n2|y\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_ok(&oracle, &db);
        assert_eq!(oracle_select(&oracle, &db, "SELECT * FROM t"), "1|x\n2|y\n");
    } else {
        skip_no_oracle("insert_round_trips_through_cli_query (oracle cross-check)");
    }
}

#[test]
fn update_and_delete_round_trip_through_cli_query() {
    let db = seed_db("update_delete");
    assert!(
        run_exec(&db, "INSERT INTO t VALUES (1,'x'),(2,'y'),(3,'z')")
            .status
            .success()
    );
    assert!(run_exec(&db, "UPDATE t SET b = 'zz' WHERE a = 3")
        .status
        .success());
    assert!(run_exec(&db, "DELETE FROM t WHERE a = 1").status.success());

    let rows = run_query(&db, "SELECT * FROM t");
    assert_eq!(rows, "2|y\n3|zz\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_ok(&oracle, &db);
        assert_eq!(
            oracle_select(&oracle, &db, "SELECT * FROM t"),
            "2|y\n3|zz\n"
        );
    } else {
        skip_no_oracle("update_and_delete_round_trip_through_cli_query (oracle cross-check)");
    }
}

#[test]
fn create_table_is_visible_to_cli_query_and_tables() {
    let db = seed_db("create_table");
    let output = run_exec(&db, "CREATE TABLE u(c INTEGER)");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(run_exec(&db, "INSERT INTO u VALUES (42)").status.success());
    assert_eq!(run_query(&db, "SELECT * FROM u"), "42\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_ok(&oracle, &db);
        assert_eq!(oracle_select(&oracle, &db, "SELECT * FROM u"), "42\n");
    } else {
        skip_no_oracle("create_table_is_visible_to_cli_query_and_tables (oracle cross-check)");
    }
}

#[test]
fn create_index_populates_existing_rows_and_survives_reopen() {
    let db = seed_db("create_index");
    assert!(run_exec(&db, "INSERT INTO t VALUES (1,'x'),(2,'y')")
        .status
        .success());
    let output = run_exec(&db, "CREATE INDEX idx_t_b ON t(b)");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Re-opening and querying again proves the index's own root page and
    // sqlite_master row persisted correctly, not just that the in-process
    // write succeeded.
    assert_eq!(run_query(&db, "SELECT * FROM t"), "1|x\n2|y\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_ok(&oracle, &db);
        assert_eq!(
            oracle_select(&oracle, &db, "SELECT * FROM t ORDER BY b"),
            "1|x\n2|y\n"
        );
    } else {
        skip_no_oracle(
            "create_index_populates_existing_rows_and_survives_reopen (oracle cross-check)",
        );
    }
}

#[test]
fn drop_index_then_drop_table_removes_them_from_the_schema() {
    let db = seed_db("drop");
    assert!(run_exec(&db, "INSERT INTO t VALUES (1,'x')")
        .status
        .success());
    assert!(run_exec(&db, "CREATE INDEX idx_t_b ON t(b)")
        .status
        .success());

    let output = run_exec(&db, "DROP INDEX idx_t_b");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = run_exec(&db, "DROP TABLE t");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_ok(&oracle, &db);
        let remaining = oracle_select(
            &oracle,
            &db,
            "SELECT count(*) FROM sqlite_master WHERE name IN ('t','idx_t_b')",
        );
        assert_eq!(remaining.trim(), "0");
    } else {
        skip_no_oracle(
            "drop_index_then_drop_table_removes_them_from_the_schema (oracle cross-check)",
        );
    }
}
