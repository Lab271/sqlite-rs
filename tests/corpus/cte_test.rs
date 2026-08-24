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
    let dir =
        std::env::temp_dir().join(format!("sqlite-rs-cte-{label}-{}-{n}", std::process::id()));
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

// #382: additional corpus coverage beyond #375/#376's original scenario
// set — a CTE self-joined twice in one query, a CTE with its own
// ORDER BY/LIMIT, and a CTE whose body is itself a compound (UNION)
// SELECT.

/// The same CTE named twice in one `FROM`/`JOIN` (a self-join) — each
/// reference must materialize/scan independently, like two real-table
/// references would.
#[test]
fn with_clause_cte_referenced_twice_self_join_matches_oracle() {
    let db = cte_fixture_db("self_join");
    assert_matches_oracle(
        &db,
        "WITH cte AS (SELECT id, x FROM t) \
         SELECT c1.id, c2.id FROM cte c1 JOIN cte c2 ON c2.x = c1.x + 10 \
         ORDER BY c1.id",
        "with_clause_cte_referenced_twice_self_join_matches_oracle",
    );
}

/// An `ORDER BY`/`LIMIT` inside the CTE's own query body — the CTE
/// materializes only its own limited/ordered result, not the whole
/// underlying table.
#[test]
fn with_clause_cte_with_internal_order_by_limit_matches_oracle() {
    let db = cte_fixture_db("order_limit");
    assert_matches_oracle(
        &db,
        "WITH cte AS (SELECT id, x FROM t ORDER BY x DESC LIMIT 2) \
         SELECT * FROM cte ORDER BY id",
        "with_clause_cte_with_internal_order_by_limit_matches_oracle",
    );
}

/// A CTE whose body is itself a compound `UNION` `SELECT` — CTE
/// materialization (#375/#376) does not yet compose with compound-
/// `SELECT` codegen (#377/#378): `materialize_from_subquery` only
/// scans a subquery body's `first` arm, silently dropping every other
/// arm's rows (a real data-loss bug found while writing this test —
/// see `src/codegen/subquery/from_clause.rs::materialize_from_subquery`,
/// fixed here to reject cleanly instead of returning wrong results;
/// full support for a compound CTE/view body is a fast-follow). This
/// pins the current clean-rejection behavior.
#[test]
fn with_clause_cte_body_is_union_is_rejected_cleanly() {
    let db = cte_fixture_db("union_body");
    let output = run_query(
        &db,
        "WITH cte AS (SELECT x FROM t WHERE x > 15 UNION SELECT x FROM t WHERE x < 25) \
         SELECT * FROM cte ORDER BY x",
    );
    assert!(
        !output.status.success(),
        "expected a compound CTE body to be rejected (not yet supported), got success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not yet supported"),
        "expected a clean 'not yet supported' rejection, got: {stderr}"
    );
}

/// A CTE referenced from more than one arm of a `UNION ALL`/`UNION`
/// compound `SELECT` (#424) — `compile_select_compound`'s per-arm
/// `compile_arm` used to unconditionally `OpenRead` a resolved-table
/// root page, ignoring `TableRefKind::Subquery` entirely, so a CTE
/// reference in *any* arm hit "table cte has an invalid root page (0)"
/// instead of being materialized like the single-`SELECT` path does.
#[test]
fn with_clause_cte_referenced_from_union_all_arms_matches_oracle() {
    let db = cte_fixture_db("union_all_arms");
    assert_matches_oracle(
        &db,
        "WITH cte AS (SELECT id, x FROM t) \
         SELECT * FROM cte UNION ALL SELECT * FROM cte",
        "with_clause_cte_referenced_from_union_all_arms_matches_oracle",
    );
}

/// Same shape as above but with a plain `UNION` (dedup) and three arms,
/// exercising the dedup ephemeral index alongside per-arm materialization.
#[test]
fn with_clause_cte_referenced_from_three_union_arms_matches_oracle() {
    let db = cte_fixture_db("union_three_arms");
    assert_matches_oracle(
        &db,
        "WITH cte AS (SELECT id, x FROM t) \
         SELECT * FROM cte UNION SELECT * FROM cte UNION SELECT * FROM cte",
        "with_clause_cte_referenced_from_three_union_arms_matches_oracle",
    );
}

/// A CTE referenced from inside an inline derived table's own `FROM`
/// (`FROM (SELECT ... FROM cte) sub`), not just directly under the main
/// query's `FROM` — `substitute_table_ref` must recurse into
/// `TableRefKind::Subquery`, the same way view expansion already does.
#[test]
fn with_clause_cte_referenced_inside_derived_table_matches_oracle() {
    let db = cte_fixture_db("derived_table");
    assert_matches_oracle(
        &db,
        "WITH cte AS (SELECT id, x FROM t) \
         SELECT * FROM (SELECT id, x FROM cte WHERE x > 15) sub ORDER BY id",
        "with_clause_cte_referenced_inside_derived_table_matches_oracle",
    );
}

/// A CTE name shadows a real table of the same name for the scope of
/// its declaring `SELECT` — `FROM cte` must resolve to the CTE's body,
/// not the real table.
#[test]
fn with_clause_cte_shadows_real_table_matches_oracle() {
    let db = cte_fixture_db("shadow");
    let create = "CREATE TABLE cte(id INTEGER PRIMARY KEY, x INTEGER)";
    if let Some(oracle) = pinned_oracle() {
        let status = Command::new(&oracle).arg(&db).arg(create).status().unwrap();
        assert!(status.success(), "oracle setup failed: {create}");
    } else {
        let output = run_exec(&db, create);
        assert!(
            output.status.success(),
            "setup {create:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let insert = "INSERT INTO cte VALUES (999, 999)";
    if let Some(oracle) = pinned_oracle() {
        let status = Command::new(&oracle).arg(&db).arg(insert).status().unwrap();
        assert!(status.success(), "oracle setup failed: {insert}");
    } else {
        let output = run_exec(&db, insert);
        assert!(
            output.status.success(),
            "setup {insert:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_matches_oracle(
        &db,
        "WITH cte AS (SELECT id, x FROM t WHERE x > 15) SELECT * FROM cte ORDER BY id",
        "with_clause_cte_shadows_real_table_matches_oracle",
    );
}

/// `INSERT INTO t WITH cte AS (...) SELECT ...` — a `WITH`-clause
/// source for `INSERT ... SELECT` — resolves the CTE reference (the
/// same `expand_with_clause` pass `compile_select_program` runs for a
/// plain `SELECT`) but is then cleanly rejected, since `compile_insert`'s
/// scan path doesn't yet drive #257's FROM-subquery materialization the
/// way a plain SELECT's codegen does. This pins the clear, explicit
/// rejection message over the confusing "invalid root page (0)" a CTE's
/// synthetic schema would otherwise surface.
#[test]
fn insert_select_with_clause_source_is_rejected_cleanly() {
    let db = cte_fixture_db("insert_with_clause");
    let output = run_exec(&db, "CREATE TABLE dst(id INTEGER, x INTEGER)");
    assert!(output.status.success());
    let output = run_exec(
        &db,
        "INSERT INTO dst WITH cte AS (SELECT id, x FROM t WHERE x > 15) SELECT * FROM cte",
    );
    assert!(
        !output.status.success(),
        "expected INSERT...WITH...SELECT to be rejected, got success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not yet supported"),
        "expected a clean 'not yet supported' rejection, got: {stderr}"
    );
}
