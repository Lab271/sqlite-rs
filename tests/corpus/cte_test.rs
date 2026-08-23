//! End-to-end oracle-diff tests for non-recursive `WITH`-clause (CTE)
//! materialization (#376) — via the `sqlite-rs` CLI's `exec`/`query`
//! subcommands, mirroring `subquery_test.rs`'s scratch-db-per-test
//! shape (a CTE reference in `FROM` is rewritten into exactly the same
//! `TableRefKind::Subquery` shape #257's `FROM`-subquery-in-derived-table
//! support already materializes and scans).

use crate::oracle::{pinned_oracle, skip_no_oracle};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-cte-{label}-{}-{n}",
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

fn assert_matches_oracle(db: &Path, sql: &str, test_name: &str) {
    let ours = run_query_ok(db, sql);
    if let Some(oracle) = pinned_oracle() {
        let theirs = oracle_select(&oracle, db, sql);
        assert_eq!(ours, theirs, "mismatch for {sql:?}");
    } else {
        skip_no_oracle(test_name);
    }
}

/// A fresh scratch db with a `t` (main) and `other` (join target) table
/// — same oracle-if-available, else-bootstrap-via-`exec` shape as
/// `subquery_test.rs`'s `subquery_fixture_db`.
fn cte_fixture_db(label: &str) -> PathBuf {
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

// #376: non-recursive `WITH`-clause CTEs, materialized into an
// ephemeral table and scanned like any other `FROM` table (#257's
// existing subquery-in-FROM machinery, reused rather than duplicated).

/// Scenario (a): the main query's `FROM` names a single CTE.
#[test]
fn with_clause_single_cte_matches_oracle() {
    let db = cte_fixture_db("single");
    assert_matches_oracle(
        &db,
        "WITH cte AS (SELECT id, x FROM t WHERE x > 15) SELECT * FROM cte ORDER BY id",
        "with_clause_single_cte_matches_oracle",
    );
}

/// Scenario (b): a CTE with an explicit `(col, ...)` column list renames
/// its output columns.
#[test]
fn with_clause_explicit_column_list_matches_oracle() {
    let db = cte_fixture_db("columns");
    assert_matches_oracle(
        &db,
        "WITH cte(a, b) AS (SELECT id, x FROM t) SELECT a, b FROM cte ORDER BY a",
        "with_clause_explicit_column_list_matches_oracle",
    );
}

/// Scenario (c): a CTE referenced in a `JOIN`, filtered further by the
/// main query's `WHERE` clause.
#[test]
fn with_clause_cte_joined_and_filtered_matches_oracle() {
    let db = cte_fixture_db("join");
    assert_matches_oracle(
        &db,
        "WITH cte AS (SELECT id, x FROM t) SELECT cte.id, other.a_id FROM cte \
         JOIN other ON other.a_id = cte.id WHERE cte.x < 25 ORDER BY cte.id",
        "with_clause_cte_joined_and_filtered_matches_oracle",
    );
}

/// Scenario (d): a second CTE in the same `WITH` clause references the
/// first one by name — non-recursive chaining.
#[test]
fn with_clause_second_cte_references_first_matches_oracle() {
    let db = cte_fixture_db("chained");
    assert_matches_oracle(
        &db,
        "WITH a AS (SELECT id, x FROM t WHERE x > 10), \
              b AS (SELECT * FROM a WHERE x < 30) \
         SELECT * FROM b ORDER BY id",
        "with_clause_second_cte_references_first_matches_oracle",
    );
}
