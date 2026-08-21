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

// Note: plain aggregates and GROUP BY work at the top level (#239/#242/
// #263), but an aggregate call inside a subquery's SELECT list is still
// rejected (`compile_value`'s `is_aggregate_call` guard) — tracked by
// #304. These tests use a non-aggregate scalar subquery rather than the
// issue's illustrative `max(x)` example until that lands.
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
/// decision made once schema information is available). Correlated
/// subqueries (the issue's "Correlated subqueries work" acceptance
/// criterion) work under materialization for free: the subquery's own
/// `Scope` falls back to the enclosing scope for any reference it can't
/// resolve itself, and since the subquery's codegen is inlined at the
/// exact point it's evaluated, it naturally re-runs once per outer row
/// with that row's outer cursor already correctly positioned — see
/// `src/codegen/subquery.rs`'s module doc comment.
#[test]
fn correlated_exists_matches_oracle() {
    let db = subquery_fixture_db("correlated_exists");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE EXISTS (SELECT 1 FROM other WHERE other.a_id = t.id)",
        "correlated_exists_matches_oracle",
    );
}

#[test]
fn correlated_not_exists_matches_oracle() {
    let db = subquery_fixture_db("correlated_not_exists");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE NOT EXISTS (SELECT 1 FROM other WHERE other.a_id = t.id)",
        "correlated_not_exists_matches_oracle",
    );
}

#[test]
fn correlated_scalar_subquery_matches_oracle() {
    let db = subquery_fixture_db("correlated_scalar");
    assert_matches_oracle(
        &db,
        "SELECT id, (SELECT other.id FROM other WHERE other.a_id = t.id) FROM t",
        "correlated_scalar_subquery_matches_oracle",
    );
}

/// `IN (SELECT ...)` materializes an ephemeral index per evaluation —
/// correlated here means that materialization re-runs (and the
/// ephemeral table is rebuilt from scratch, see `OpenEphemeral`'s
/// execution) once per outer row, since the correlated reference makes
/// each row's subquery result potentially different.
#[test]
fn correlated_in_subquery_matches_oracle() {
    let db = subquery_fixture_db("correlated_in");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE id IN (SELECT other.a_id FROM other WHERE other.a_id = t.id)",
        "correlated_in_subquery_matches_oracle",
    );
}

/// `ANY`/`ALL`/`SOME` quantified comparisons and subqueries in `FROM`
/// stay out of scope for now — must fail cleanly as unsupported/
/// invalid, not panic. Flipped one at a time as #251's sub-items land.
#[test]
fn still_unsupported_subquery_forms_fail_cleanly() {
    let db = subquery_fixture_db("still_unsupported");
    for sql in [
        "SELECT id FROM t WHERE x > ANY (SELECT x FROM t)",
        "SELECT id FROM t WHERE x > ALL (SELECT x FROM t)",
    ] {
        let output = run_query(&db, sql);
        assert!(!output.status.success(), "expected {sql:?} to fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("panicked at"), "must not panic: {stderr}");
    }
}

/// #251: multi-column `IN (SELECT ...)` materializes the subquery's
/// projected columns into an N-key ephemeral index.
#[test]
fn multi_column_in_subquery_matches_oracle() {
    let db = subquery_fixture_db("multi_col_in");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE (id, x) IN (SELECT id, x FROM t WHERE id = 2)",
        "multi_column_in_subquery_matches_oracle",
    );
}

#[test]
fn multi_column_not_in_subquery_matches_oracle() {
    let db = subquery_fixture_db("multi_col_not_in");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE (id, x) NOT IN (SELECT id, x FROM t WHERE id = 2)",
        "multi_column_not_in_subquery_matches_oracle",
    );
}

/// #268: a multi-column `IN`/`NOT IN` subquery whose result set is
/// empty — `IN` must match no rows, `NOT IN` must match every row
/// (vacuously true for all of them).
/// #268: a two-level-deep correlated subquery — the innermost subquery
/// skips its immediate parent scope and correlates against the
/// grandparent (outermost) query's `t.id`. Per this pass's scope-chain
/// fallback (see `correlated_exists_matches_oracle`'s doc comment
/// above), any depth of nesting resolves for free.
#[test]
fn two_level_correlated_exists_matches_oracle() {
    let db = subquery_fixture_db("two_level_correlated");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE EXISTS (SELECT 1 FROM other WHERE EXISTS \
         (SELECT 1 FROM other WHERE other.a_id = t.id))",
        "two_level_correlated_exists_matches_oracle",
    );
}

/// #268: a correlated subquery nested inside a FROM-subquery's SELECT
/// list is a separate, still-unimplemented shape — `src/codegen/
/// subquery.rs`'s catalog-visibility check rejects the inner
/// subquery's reference to `other` because a FROM-subquery's own SELECT
/// list is compiled against just that subquery's schema, not the full
/// outer catalog. Documents the current clean rejection; not fixed
/// here (tracked as a follow-on feature gap).
#[test]
fn correlated_subquery_inside_from_subquery_select_list_is_still_unsupported() {
    let db = subquery_fixture_db("from_subquery_correlated_select_list");
    let output = run_query(
        &db,
        "SELECT * FROM (SELECT id, (SELECT a_id FROM other WHERE other.id = t.id + 99) AS sub \
         FROM t) AS s",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "expected this form to fail");
    assert!(
        stderr.contains("isn't visible to this compiler's"),
        "expected the catalog-visibility diagnostic; got: {stderr}"
    );
    assert!(!stderr.contains("panicked at"), "must not panic: {stderr}");
}

#[test]
fn multi_column_in_subquery_zero_rows_matches_oracle() {
    let db = subquery_fixture_db("multi_col_in_zero_rows");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE (id, x) IN (SELECT id, x FROM t WHERE id = 999)",
        "multi_column_in_subquery_zero_rows_matches_oracle",
    );
}

#[test]
fn multi_column_not_in_subquery_zero_rows_matches_oracle() {
    let db = subquery_fixture_db("multi_col_not_in_zero_rows");
    assert_matches_oracle(
        &db,
        "SELECT id FROM t WHERE (id, x) NOT IN (SELECT id, x FROM t WHERE id = 999)",
        "multi_column_not_in_subquery_zero_rows_matches_oracle",
    );
}

/// #268: three-valued-logic edge case — a `NULL` component inside one
/// of the subquery's result tuples. Per SQL semantics, `NOT IN` against
/// a subquery result set containing any `NULL` component in the
/// compared position can never yield a *known-true* `NOT IN`, so it
/// must produce zero rows (not a crash, and not treating the `NULL` as
/// an ordinary non-matching value).
#[test]
fn multi_column_not_in_subquery_with_null_component_matches_oracle() {
    let db = scratch_db("multi_col_not_in_null");
    let ddls = ["CREATE TABLE u(id INTEGER PRIMARY KEY, x INTEGER)"];
    let rows = ["INSERT INTO u VALUES (1, 10), (2, NULL), (3, 30)"];
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
    assert_matches_oracle(
        &db,
        "SELECT id FROM u WHERE (id, x) NOT IN (SELECT id, x FROM u)",
        "multi_column_not_in_subquery_with_null_component_matches_oracle",
    );
}

/// #251: UPDATE's `SET`/`WHERE` clauses now thread the full table
/// catalog through, so a subquery referencing a table other than the
/// target resolves instead of failing at codegen time.
#[test]
fn update_set_scalar_subquery_matches_oracle() {
    let db = subquery_fixture_db("update_set_scalar");
    run_exec(
        &db,
        "UPDATE t SET x = (SELECT id FROM other WHERE other.a_id = t.id) WHERE id = 1",
    );
    assert_matches_oracle(
        &db,
        "SELECT id, x FROM t ORDER BY id",
        "update_set_scalar_subquery_matches_oracle",
    );
}

#[test]
fn update_where_in_subquery_matches_oracle() {
    let db = subquery_fixture_db("update_where_in");
    let output = run_exec(
        &db,
        "UPDATE t SET x = 0 WHERE id IN (SELECT a_id FROM other)",
    );
    assert!(
        output.status.success(),
        "UPDATE failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_matches_oracle(
        &db,
        "SELECT id, x FROM t ORDER BY id",
        "update_where_in_subquery_matches_oracle",
    );
}

/// #251: DELETE's `WHERE` clause now threads the full table catalog
/// through as well.
#[test]
fn delete_where_in_subquery_matches_oracle() {
    let db = subquery_fixture_db("delete_where_in");
    let output = run_exec(&db, "DELETE FROM t WHERE id IN (SELECT a_id FROM other)");
    assert!(
        output.status.success(),
        "DELETE failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_matches_oracle(
        &db,
        "SELECT id, x FROM t ORDER BY id",
        "delete_where_in_subquery_matches_oracle",
    );
}

#[test]
fn delete_where_exists_correlated_matches_oracle() {
    let db = subquery_fixture_db("delete_where_exists");
    let output = run_exec(
        &db,
        "DELETE FROM t WHERE EXISTS (SELECT 1 FROM other WHERE other.a_id = t.id)",
    );
    assert!(
        output.status.success(),
        "DELETE failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_matches_oracle(
        &db,
        "SELECT id, x FROM t ORDER BY id",
        "delete_where_exists_correlated_matches_oracle",
    );
}

// #257: subqueries in FROM — materialized into an ephemeral table and
// scanned like any other FROM table.

/// Acceptance criterion 1: a single-table subquery in FROM.
#[test]
fn subquery_in_from_single_table_matches_oracle() {
    let db = subquery_fixture_db("from_subquery_single");
    assert_matches_oracle(
        &db,
        "SELECT * FROM (SELECT id, x FROM t WHERE x > 15) AS sub ORDER BY id",
        "subquery_in_from_single_table_matches_oracle",
    );
}

/// Acceptance criterion 2: a joined outer query with a subquery in one
/// FROM slot.
#[test]
fn subquery_in_from_joined_outer_matches_oracle() {
    let db = subquery_fixture_db("from_subquery_joined_outer");
    assert_matches_oracle(
        &db,
        "SELECT sub.id, other.a_id FROM (SELECT id, x FROM t WHERE x > 10) AS sub \
         JOIN other ON other.a_id = sub.id ORDER BY sub.id",
        "subquery_in_from_joined_outer_matches_oracle",
    );
}

/// Acceptance criterion 3: a subquery in FROM whose own FROM clause has
/// a JOIN.
#[test]
fn subquery_in_from_own_join_matches_oracle() {
    let db = subquery_fixture_db("from_subquery_own_join");
    assert_matches_oracle(
        &db,
        "SELECT * FROM (SELECT t.id, other.a_id FROM t JOIN other ON other.a_id = t.id) AS sub \
         ORDER BY sub.id",
        "subquery_in_from_own_join_matches_oracle",
    );
}
