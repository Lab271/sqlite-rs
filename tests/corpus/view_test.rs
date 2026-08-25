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
    let dir =
        std::env::temp_dir().join(format!("sqlite-rs-view-{label}-{}-{n}", std::process::id()));
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
    assert_matches_oracle(
        &db,
        "SELECT * FROM v ORDER BY id",
        "create_view_simple_matches_oracle",
    );
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
    assert_matches_oracle(
        &db,
        "SELECT * FROM v2 ORDER BY id",
        "create_view_of_view_matches_oracle",
    );
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
    assert_matches_oracle(
        &db,
        "SELECT * FROM v ORDER BY id",
        "create_view_persists_across_reload",
    );
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

/// A view whose body is itself a compound `UNION` `SELECT` — the same
/// `materialize_from_subquery` guard that rejects a compound CTE body
/// (`cte_test.rs::with_clause_cte_body_is_union_is_rejected_cleanly`)
/// is shared by view expansion, so this must fail cleanly too rather
/// than silently scanning only the view's first arm.
#[test]
fn create_view_body_is_union_is_rejected_cleanly() {
    let db = view_fixture_db("union_body");
    run_exec_ok(
        &db,
        "CREATE VIEW v AS SELECT x FROM t WHERE x > 15 UNION SELECT x FROM t WHERE x < 25",
    );
    let output = run_query(&db, "SELECT * FROM v ORDER BY x");
    assert!(
        !output.status.success(),
        "expected a compound view body to be rejected (not yet supported), got success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not yet supported"),
        "expected a clean 'not yet supported' rejection, got: {stderr}"
    );
}

/// `INSERT INTO t SELECT * FROM some_view` — a view-backed source for
/// `INSERT ... SELECT` — resolves the view reference (the same
/// `expand_views` pass `compile_select_program` runs for a plain
/// `SELECT`) but is then cleanly rejected, since `compile_insert`'s scan
/// path doesn't yet drive #257's FROM-subquery materialization the way
/// a plain SELECT's codegen does. This pins the clear, explicit
/// rejection message over the confusing "invalid root page (0)" a
/// view's synthetic schema would otherwise surface.
#[test]
fn insert_select_from_view_is_rejected_cleanly() {
    let db = view_fixture_db("insert_from_view");
    run_exec_ok(&db, "CREATE TABLE dst(id INTEGER, x INTEGER)");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT id, x FROM t WHERE x > 15");
    let output = run_exec(&db, "INSERT INTO dst SELECT * FROM v");
    assert!(
        !output.status.success(),
        "expected INSERT...SELECT FROM a view to be rejected, got success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not yet supported"),
        "expected a clean 'not yet supported' rejection, got: {stderr}"
    );
}

/// The mirror image of `with_clause_cte_selects_from_view_matches_oracle`
/// (`cte_test.rs`): a view's own body starts with a `WITH` clause. View
/// expansion must run `expand_with_clause` on the substituted body
/// before recursing into further view references, since only the
/// outermost query gets that pass otherwise.
#[test]
fn create_view_body_selects_from_cte_matches_oracle() {
    let db = view_fixture_db("view_from_cte");
    run_exec_ok(
        &db,
        "CREATE VIEW v AS WITH cte AS (SELECT id, x FROM t WHERE x > 10) \
         SELECT * FROM cte WHERE x < 30",
    );
    assert_matches_oracle(
        &db,
        "SELECT * FROM v ORDER BY id",
        "create_view_body_selects_from_cte_matches_oracle",
    );
}

/// A view directly referencing itself must be rejected cleanly with a
/// "circularly defined" error (matching stock SQLite's own wording),
/// not recurse forever or silently mis-resolve.
#[test]
fn create_view_self_reference_is_rejected_cleanly() {
    let db = view_fixture_db("self_ref");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT * FROM v");
    let output = run_query(&db, "SELECT * FROM v");
    assert!(
        !output.status.success(),
        "expected a self-referencing view to be rejected, got success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("circularly defined"),
        "expected a 'circularly defined' rejection, got: {stderr}"
    );
}

// #532: predicate push-down into FROM-subqueries/views — an outer WHERE
// conjunct that resolves solely against a pushdown-safe view's own
// (identity-mapped) output columns is moved into the view's own WHERE
// clause before it materializes, so an index on the underlying table can
// be used. These are oracle-diff correctness checks (does pushing the
// predicate change which rows come back?); `eqp_test.rs` covers the
// actual plan-shape/index-selection assertion.

/// A plain `SELECT * FROM t` view — every column is identity-mapped, so
/// the outer `WHERE` should push straight through to the base table.
#[test]
fn predicate_pushdown_into_simple_view_matches_oracle() {
    let db = view_fixture_db("pushdown_simple");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT * FROM t");
    assert_matches_oracle(
        &db,
        "SELECT * FROM v WHERE x > 15 ORDER BY id",
        "predicate_pushdown_into_simple_view_matches_oracle",
    );
}

/// A view with a renamed output column (`CREATE VIEW v(a, b)`) is still
/// identity-mapped — the pushed predicate must be rewritten against the
/// view body's own underlying column name, not the outer alias.
#[test]
fn predicate_pushdown_into_renamed_column_view_matches_oracle() {
    let db = view_fixture_db("pushdown_renamed");
    run_exec_ok(&db, "CREATE VIEW v (a, b) AS SELECT id, x FROM t");
    assert_matches_oracle(
        &db,
        "SELECT * FROM v WHERE b > 15 ORDER BY a",
        "predicate_pushdown_into_renamed_column_view_matches_oracle",
    );
}

/// A view whose body is itself filtered — the pushed-down outer
/// predicate must combine (AND) with the view's own `WHERE`, not replace
/// it.
#[test]
fn predicate_pushdown_combines_with_views_own_where_matches_oracle() {
    let db = view_fixture_db("pushdown_combine");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT id, x FROM t WHERE x > 10");
    assert_matches_oracle(
        &db,
        "SELECT * FROM v WHERE x < 25 ORDER BY id",
        "predicate_pushdown_combines_with_views_own_where_matches_oracle",
    );
}

/// A `DISTINCT` view is unsafe to push a predicate into ahead of the
/// dedup — this pins that pushdown is correctly suppressed (an oracle
/// mismatch would be the failure mode if it weren't).
#[test]
fn predicate_pushdown_skips_distinct_view_matches_oracle() {
    let db = view_fixture_db("pushdown_distinct");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT DISTINCT x FROM t");
    assert_matches_oracle(
        &db,
        "SELECT * FROM v WHERE x > 15 ORDER BY x",
        "predicate_pushdown_skips_distinct_view_matches_oracle",
    );
}

/// A predicate referencing a *computed* view column (no single
/// underlying column to rewrite against) must stay outer, untouched.
#[test]
fn predicate_pushdown_skips_computed_view_column_matches_oracle() {
    let db = view_fixture_db("pushdown_computed");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT id, x * 2 AS doubled FROM t");
    assert_matches_oracle(
        &db,
        "SELECT * FROM v WHERE doubled > 30 ORDER BY id",
        "predicate_pushdown_skips_computed_view_column_matches_oracle",
    );
}

/// A predicate pushed into a nested view (a view of a view) must keep
/// chaining inward.
#[test]
fn predicate_pushdown_chains_through_nested_views_matches_oracle() {
    let db = view_fixture_db("pushdown_nested");
    run_exec_ok(&db, "CREATE VIEW v1 AS SELECT id, x FROM t");
    run_exec_ok(&db, "CREATE VIEW v2 AS SELECT id, x FROM v1");
    assert_matches_oracle(
        &db,
        "SELECT * FROM v2 WHERE x > 15 ORDER BY id",
        "predicate_pushdown_chains_through_nested_views_matches_oracle",
    );
}

/// #532's `EXPLAIN QUERY PLAN` acceptance criterion: an `x = 15`
/// predicate on a `SELECT x FROM t` view (the projection an index on `x`
/// alone can cover) must nest a `SEARCH` row (naming the pushed-down
/// predicate's index) underneath the outer `SCAN (subquery)` row —
/// proving the predicate actually reached the view body's own `WHERE`
/// clause before it materializes, not just that results are correct.
#[test]
fn eqp_reports_predicate_pushed_into_view_uses_index() {
    let db = view_fixture_db("eqp_pushdown_view");
    run_exec_ok(&db, "CREATE INDEX t_x ON t(x)");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT x FROM t");
    let output = run_query(&db, "EXPLAIN QUERY PLAN SELECT * FROM v WHERE x = 15");
    let output = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.contains("SCAN (subquery) AS v"),
        "expected the outer row to report scanning the materialized view, got: {output}"
    );
    assert!(
        output.contains("SEARCH t USING") && output.contains("INDEX t_x"),
        "expected a nested SEARCH row naming the pushed-down index t_x, got: {output}"
    );
}

/// #535's fix (a rowid-alias `INTEGER PRIMARY KEY` column is free from
/// any index leaf, so it doesn't block a covering-index scan) composes
/// with #532's pushdown: a bare `SELECT * FROM t` view — needing both
/// `id` (the rowid alias) and `x` (the index's own column) — must also
/// nest a covering-index `SEARCH` row, not fall back to a full scan.
#[test]
fn eqp_reports_predicate_pushed_into_star_view_uses_covering_index() {
    let db = view_fixture_db("eqp_pushdown_star_view");
    run_exec_ok(&db, "CREATE INDEX t_x ON t(x)");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT * FROM t");
    let output = run_query(&db, "EXPLAIN QUERY PLAN SELECT * FROM v WHERE x = 15");
    let output = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.contains("SCAN (subquery) AS v"),
        "expected the outer row to report scanning the materialized view, got: {output}"
    );
    assert!(
        output.contains("SEARCH t USING COVERING INDEX t_x"),
        "expected a nested covering-index SEARCH row for t_x, got: {output}"
    );
}

/// The `DISTINCT`-view mirror of the test above: pushdown must be
/// suppressed, so the nested row for `t` stays a plain `SCAN` even
/// though `x` has an index — filtering ahead of the dedup would change
/// results, not just plan shape.
#[test]
fn eqp_reports_no_pushdown_for_distinct_view() {
    let db = view_fixture_db("eqp_no_pushdown_distinct");
    run_exec_ok(&db, "CREATE INDEX t_x ON t(x)");
    run_exec_ok(&db, "CREATE VIEW v AS SELECT DISTINCT x FROM t");
    let output = run_query(&db, "EXPLAIN QUERY PLAN SELECT * FROM v WHERE x = 15");
    let output = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.contains("SCAN t"),
        "expected the nested row for t to stay a full SCAN (no unsafe pushdown), got: {output}"
    );
}

/// Two views that reference each other (a longer cycle than direct
/// self-reference) must also be rejected cleanly.
#[test]
fn create_view_mutual_reference_is_rejected_cleanly() {
    let db = view_fixture_db("mutual_ref");
    run_exec_ok(&db, "CREATE VIEW v1 AS SELECT * FROM v2");
    run_exec_ok(&db, "CREATE VIEW v2 AS SELECT * FROM v1");
    let output = run_query(&db, "SELECT * FROM v1");
    assert!(
        !output.status.success(),
        "expected a mutually-recursive view pair to be rejected, got success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("circularly defined"),
        "expected a 'circularly defined' rejection, got: {stderr}"
    );
}
