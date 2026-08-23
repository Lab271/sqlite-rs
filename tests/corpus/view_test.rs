//! End-to-end oracle-diff tests for `CREATE VIEW`/`DROP VIEW` storage
//! and query expansion (#380) — via the `sqlite-rs` CLI's `exec`/`query`
//! subcommands, mirroring `cte_test.rs`'s scratch-db-per-test shape. A
//! view is registered in `sqlite_master` (`type = 'view'`, `rootpage =
//! 0`, verbatim source text) exactly like real SQLite, so an
//! oracle-diff test can run `CREATE VIEW` through our own `exec`
//! binary and then query the *same on-disk file* through either engine
//! — unlike a CTE (purely an in-query AST rewrite), a view's
//! queryability must survive being reloaded from `sqlite_master` by a
//! fresh process, which is the scenario this file's persistence test
//! specifically exercises.

use crate::oracle::{pinned_oracle, skip_no_oracle};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-view-{label}-{}-{n}",
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

fn run_exec_ok(db: &Path, sql: &str) {
    let output = run_exec(db, sql);
    assert!(
        output.status.success(),
        "exec {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
/// `cte_test.rs`'s `cte_fixture_db`.
fn view_fixture_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    let ddls = [
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
        "CREATE TABLE other(id INTEGER PRIMARY KEY, t_id INTEGER)",
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
        run_exec_ok(&db, "CREATE TABLE seed_bootstrap(x)");
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

// #380: `CREATE VIEW` storage in `sqlite_master` plus `FROM`-clause
// expansion into a `TableRefKind::Subquery` (#257's existing
// subquery-in-FROM materialization, the same mechanism #376's CTEs
// ride on).

/// Scenario (a): create + query a simple view.
#[test]
fn create_view_simple_matches_oracle() {
    let db = view_fixture_db("simple");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT id, x FROM t WHERE x > 15");
    assert_matches_oracle(&db, "SELECT * FROM v ORDER BY id", "create_view_simple_matches_oracle");
}

/// Scenario (b): a view with an explicit `(col, ...)` list renames its
/// output columns.
#[test]
fn create_view_explicit_column_list_matches_oracle() {
    let db = view_fixture_db("columns");
    run_exec_ok(&db, "CREATE VIEW v (a, b) AS SELECT id, x FROM t");
    assert_matches_oracle(
        &db,
        "SELECT a, b FROM v ORDER BY a",
        "create_view_explicit_column_list_matches_oracle",
    );
}

/// Scenario (c): a nested view (view of a view).
#[test]
fn create_view_of_view_matches_oracle() {
    let db = view_fixture_db("nested");
    run_exec_ok(&db, "CREATE VIEW v1 AS SELECT id, x FROM t WHERE x > 10");
    run_exec_ok(&db, "CREATE VIEW v2 AS SELECT id, x FROM v1 WHERE x < 30");
    assert_matches_oracle(&db, "SELECT * FROM v2 ORDER BY id", "create_view_of_view_matches_oracle");
}

/// Scenario (d): a view referenced in a `JOIN`, filtered further by the
/// main query's `WHERE` clause.
#[test]
fn create_view_joined_and_filtered_matches_oracle() {
    let db = view_fixture_db("join");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT id, x FROM t");
    assert_matches_oracle(
        &db,
        "SELECT v.id, other.t_id FROM v JOIN other ON other.t_id = v.id \
         WHERE v.x < 25 ORDER BY v.id",
        "create_view_joined_and_filtered_matches_oracle",
    );
}

/// A view survives being reloaded from `sqlite_master` by a fresh
/// process — the main functional difference from a CTE (a pure
/// in-query AST rewrite that never touches storage). `run_exec`/
/// `run_query` are already separate CLI invocations (fresh processes,
/// no shared in-memory state), so simply creating the view in one
/// invocation and querying it in another already proves this; this
/// test also re-verifies `sqlite_master`'s stored `rootpage` is `0`
/// (views have no b-tree of their own) via the oracle, when available.
#[test]
fn create_view_persists_across_reload() {
    let db = view_fixture_db("persist");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT id, x FROM t WHERE x > 15");
    // Fresh `query` invocation: a new process, new `Pager`, schema
    // re-read from `sqlite_master` from scratch.
    assert_matches_oracle(&db, "SELECT * FROM v ORDER BY id", "create_view_persists_across_reload");
    if let Some(oracle) = pinned_oracle() {
        let rootpage = oracle_select(
            &oracle,
            &db,
            "SELECT rootpage FROM sqlite_master WHERE name = 'v'",
        );
        assert_eq!(rootpage.trim(), "0");
    } else {
        skip_no_oracle("create_view_persists_across_reload (rootpage check)");
    }
}

// #382: additional corpus coverage beyond #380's original scenario set
// — a `WITH`-clause CTE whose body reads from a view (per Requirement
// 15, `expand_views` runs after `expand_with_clause` so it also
// reaches into a CTE's rewritten body), and `DROP VIEW`'s current
// end-to-end behavior.

/// A CTE's body selects from a view — `expand_with_clause` runs first
/// (rewriting the CTE reference into a `TableRefKind::Subquery`
/// wrapping the CTE body verbatim, view reference and all), then
/// `expand_views` recurses into that subquery and expands the view
/// reference it finds there.
#[test]
fn with_clause_cte_selects_from_view_matches_oracle() {
    let db = view_fixture_db("cte_from_view");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT id, x FROM t WHERE x > 10");
    assert_matches_oracle(
        &db,
        "WITH cte AS (SELECT id, x FROM v WHERE x < 30) SELECT * FROM cte ORDER BY id",
        "with_clause_cte_selects_from_view_matches_oracle",
    );
}

/// `DROP VIEW` (#379's parser support) is not yet wired into codegen
/// (Requirement 15 explicitly scopes it out) — running it end-to-end
/// against a real connection MUST fail cleanly (a rejected/unsupported
/// statement error) rather than panicking or silently no-opping. This
/// pins that current behavior so a future #380 follow-on that wires
/// `Opcode::DropView` has a test to flip instead of a silent gap.
#[test]
fn drop_view_fails_cleanly_not_wired_into_codegen() {
    let db = view_fixture_db("drop_view");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT id, x FROM t");
    let output = run_exec(&db, "DROP VIEW v");
    assert!(
        !output.status.success(),
        "expected DROP VIEW to be rejected (not yet wired into codegen), \
         got success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.is_empty(),
        "expected a clean rejection message on stderr, got none"
    );
    // The view must be unaffected — still queryable after the rejected
    // DROP VIEW attempt.
    assert_matches_oracle(
        &db,
        "SELECT * FROM v ORDER BY id",
        "drop_view_fails_cleanly_not_wired_into_codegen (view still queryable)",
    );
}
