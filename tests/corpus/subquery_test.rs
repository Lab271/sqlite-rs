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

/// The `-explain` bytecode listing for `db`/`sql`, one opcode row per
/// line (`addr|opcode|p1|p2|p3|p4|p5|comment`) — used below to inspect
/// *where* an opcode landed relative to the outer scan's `Rewind`,
/// which correctness-only oracle comparison can't see.
fn explain(db: &Path, sql: &str) -> String {
    let output = Command::new(CLI)
        .arg("query")
        .arg("-explain")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} query -explain {}: {e}", db.display()));
    assert!(
        output.status.success(),
        "explain {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The 0-based line index of the first opcode row whose `opcode` column
/// is `name`, or `None`.
fn first_opcode_line(program: &str, name: &str) -> Option<usize> {
    program
        .lines()
        .position(|l| l.split('|').nth(1) == Some(name))
}

/// The 0-based line index of the outer single-table scan's own
/// `Rewind` — its `p1` (cursor) column is always `0` in every query
/// these tests compile, since `Scope::single`'s table cursor is
/// allocated first, before any subquery's own cursor (subquery cursors
/// start at 1000, `RegAlloc::next_cursor`'s default — see
/// `codegen.rs`). A subquery's own `Rewind`, in contrast, always has a
/// `p1` >= 1000, so filtering on `p1 == "0"` unambiguously picks out
/// the *outer* scan's `Rewind` even when a hoisted subquery's own
/// `Rewind` appears earlier in program order.
fn outer_rewind_line(program: &str) -> usize {
    program
        .lines()
        .position(|l| {
            let mut fields = l.split('|');
            fields.next(); // addr
            fields.next() == Some("Rewind") && fields.next() == Some("0")
        })
        .expect("expected the outer scan's own Rewind opcode (cursor 0)")
}

/// #306 regression: an uncorrelated `IN (SELECT ...)` in a single-table
/// `WHERE` clause must materialize its ephemeral membership index
/// exactly once, *before* the outer scan's `Rewind` — not on every
/// outer row. `OpenEphemeral`'s address must therefore land strictly
/// before the outer table cursor's `Rewind`, which correctness-only
/// (oracle-diff) coverage can't distinguish from the old per-row
/// placement (both compile to a correct, but for large outer tables
/// disastrously slow, result).
#[test]
fn uncorrelated_in_subquery_where_clause_hoists_ephemeral_before_outer_rewind() {
    let db = subquery_fixture_db("hoist_in");
    let program = explain(&db, "SELECT id FROM t WHERE id IN (SELECT a_id FROM other)");
    let eph_addr =
        first_opcode_line(&program, "OpenEphemeral").expect("expected an OpenEphemeral opcode");
    let rewind_addr = outer_rewind_line(&program);
    assert!(
        eph_addr < rewind_addr,
        "expected the IN-subquery's OpenEphemeral (addr {eph_addr}) to be hoisted before the \
         outer scan's Rewind (addr {rewind_addr}); program:\n{program}"
    );
}

/// #306 regression: a correlated `IN (SELECT ...)` — the subquery's own
/// `WHERE` references the outer row (`other.a_id = t.id`) — must keep
/// re-materializing per outer row: its `OpenEphemeral` stays *after* the
/// outer `Rewind`, inside the loop. Proves the hoist's correlation check
/// actually gates the optimization rather than firing unconditionally.
#[test]
fn correlated_in_subquery_where_clause_is_not_hoisted() {
    let db = subquery_fixture_db("no_hoist_in");
    let program = explain(
        &db,
        "SELECT id FROM t WHERE id IN (SELECT other.a_id FROM other WHERE other.a_id = t.id)",
    );
    let eph_addr =
        first_opcode_line(&program, "OpenEphemeral").expect("expected an OpenEphemeral opcode");
    let rewind_addr = outer_rewind_line(&program);
    assert!(
        eph_addr > rewind_addr,
        "correlated subquery's OpenEphemeral (addr {eph_addr}) must stay inside the outer \
         scan's loop, after Rewind (addr {rewind_addr}); program:\n{program}"
    );
}

/// #306 regression: an uncorrelated scalar subquery in a single-table
/// `WHERE` clause must materialize exactly once, before the outer
/// scan's `Rewind` — its inner `OpenRead` (the subquery's own table
/// cursor) lands before the outer `Rewind`, not after.
#[test]
fn uncorrelated_scalar_subquery_where_clause_hoists_before_outer_rewind() {
    let db = subquery_fixture_db("hoist_scalar");
    let program = explain(
        &db,
        "SELECT id FROM t WHERE x = (SELECT x FROM t WHERE id = 2)",
    );
    // Two `OpenRead`s total: the subquery's own (hoisted, first) and the
    // outer scan's own (second). The outer `Rewind` must come after
    // both.
    let open_read_addrs: Vec<usize> = program
        .lines()
        .enumerate()
        .filter(|(_, l)| l.split('|').nth(1) == Some("OpenRead"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        open_read_addrs.len(),
        2,
        "expected exactly 2 OpenReads (subquery + outer scan); program:\n{program}"
    );
    let rewind_addr = outer_rewind_line(&program);
    assert!(
        open_read_addrs[0] < rewind_addr && open_read_addrs[1] < rewind_addr,
        "expected both the hoisted scalar subquery's OpenRead and the outer scan's own \
         OpenRead to precede the outer Rewind (addr {rewind_addr}); program:\n{program}"
    );
}

/// #322 regression: #306's hoist was wired into `compile_direct_scan`/
/// `compile_sorted_scan` but never into `compile_grouped_scan` — so an
/// uncorrelated scalar (here, aggregate — #304) subquery in the `WHERE`
/// clause of an aggregate/`GROUP BY`-bearing outer query kept
/// re-materializing per WHERE-matching row, same class of bug as the
/// scalar-subquery case above but for the aggregate scan path. Same
/// `OpenRead`-before-`Rewind` shape as
/// `uncorrelated_scalar_subquery_where_clause_hoists_before_outer_rewind`,
/// just with an aggregate outer query (`count(*)`, no `GROUP BY` —
/// #287's implicit whole-table group, which is what routes through
/// `compile_grouped_scan`) instead of a plain projection.
#[test]
fn uncorrelated_aggregate_subquery_where_clause_hoists_before_outer_rewind() {
    let db = subquery_fixture_db("hoist_agg_subquery");
    let program = explain(
        &db,
        "SELECT count(*) FROM t WHERE x > (SELECT avg(x) FROM t)",
    );
    let open_read_addrs: Vec<usize> = program
        .lines()
        .enumerate()
        .filter(|(_, l)| l.split('|').nth(1) == Some("OpenRead"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        open_read_addrs.len(),
        2,
        "expected exactly 2 OpenReads (subquery + outer scan); program:\n{program}"
    );
    let rewind_addr = outer_rewind_line(&program);
    assert!(
        open_read_addrs[0] < rewind_addr && open_read_addrs[1] < rewind_addr,
        "expected both the hoisted aggregate subquery's OpenRead and the outer scan's own \
         OpenRead to precede the outer Rewind (addr {rewind_addr}); program:\n{program}"
    );
}

/// #323 regression: same root cause as #322 above (`compile_grouped_scan`
/// never getting #306's hoist wiring), but for the `IN (SELECT ...)`
/// shape instead of a scalar/aggregate comparison — confirmed to blow
/// the VDBE step cap on a real-sized table before this fix, in exactly
/// the way `tests/performance/engine.rs`'s `subquery` scenario comment
/// already predicted for the `IN` form. `hoist_uncorrelated_where_subqueries`
/// already handles `InSubquery` conjuncts generically (`try_hoist_conjunct`),
/// so the `compile_grouped_scan` wiring added for #322 fixes this shape
/// too, with no further codegen change — this test only proves that.
#[test]
fn uncorrelated_in_subquery_where_clause_hoists_before_outer_rewind_in_aggregate_scan() {
    let db = subquery_fixture_db("hoist_agg_in_subquery");
    let program = explain(
        &db,
        "SELECT count(*) FROM t WHERE id IN (SELECT a_id FROM other)",
    );
    let eph_addr =
        first_opcode_line(&program, "OpenEphemeral").expect("expected an OpenEphemeral opcode");
    let rewind_addr = outer_rewind_line(&program);
    assert!(
        eph_addr < rewind_addr,
        "expected the IN-subquery's OpenEphemeral (addr {eph_addr}) to be hoisted before the \
         outer aggregate scan's Rewind (addr {rewind_addr}); program:\n{program}"
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

/// #304: an aggregate call inside a scalar subquery's projection —
/// `compile_scalar_subquery` routes this through the same #287
/// implicit-whole-table-group machinery `compile_grouped_scan` gives a
/// top-level `GROUP BY`-less aggregate query, instead of
/// `compile_value`'s plain (aggregate-rejecting) expression path.
#[test]
fn aggregate_in_scalar_subquery_matches_oracle() {
    let db = subquery_fixture_db("aggregate_scalar");
    for (name, sql) in [
        ("count", "SELECT (SELECT count(x) FROM t) FROM t LIMIT 1"),
        ("sum", "SELECT (SELECT sum(x) FROM t) FROM t LIMIT 1"),
        ("avg", "SELECT (SELECT avg(x) FROM t) FROM t LIMIT 1"),
        ("min", "SELECT (SELECT min(x) FROM t) FROM t LIMIT 1"),
        ("max", "SELECT (SELECT max(x) FROM t) FROM t LIMIT 1"),
    ] {
        assert_matches_oracle(&db, sql, &format!("aggregate_in_scalar_subquery_{name}"));
    }
}

/// The correlated form of #304's aggregate-in-subquery: the
/// subquery's own `WHERE` clause references the enclosing query's
/// row (`t.id`), so the aggregate must be recomputed once per outer
/// row rather than hoisted/computed once.
#[test]
fn correlated_aggregate_in_scalar_subquery_matches_oracle() {
    let db = subquery_fixture_db("correlated_aggregate_scalar");
    for (name, sql) in [
        (
            "max",
            "SELECT id, (SELECT max(other.a_id) FROM other WHERE other.a_id = t.id) FROM t",
        ),
        (
            "count",
            "SELECT id, (SELECT count(other.a_id) FROM other WHERE other.a_id = t.id) FROM t",
        ),
    ] {
        assert_matches_oracle(
            &db,
            sql,
            &format!("correlated_aggregate_in_scalar_subquery_{name}"),
        );
    }
}

/// #304 + #287: an aggregate over a scalar subquery whose inner
/// `WHERE` matches zero rows still produces exactly one group (the
/// implicit whole-table group), with `count` = 0 and the other
/// aggregates = NULL — same zero-rows semantics #287 established at
/// the top level, now proven through the subquery-expression path too.
#[test]
fn aggregate_in_scalar_subquery_over_empty_result_matches_oracle() {
    let db = subquery_fixture_db("aggregate_scalar_empty");
    for (name, sql) in [
        (
            "count",
            "SELECT (SELECT count(x) FROM t WHERE x = 999) FROM t LIMIT 1",
        ),
        (
            "sum",
            "SELECT (SELECT sum(x) FROM t WHERE x = 999) FROM t LIMIT 1",
        ),
        (
            "avg",
            "SELECT (SELECT avg(x) FROM t WHERE x = 999) FROM t LIMIT 1",
        ),
        (
            "min",
            "SELECT (SELECT min(x) FROM t WHERE x = 999) FROM t LIMIT 1",
        ),
        (
            "max",
            "SELECT (SELECT max(x) FROM t WHERE x = 999) FROM t LIMIT 1",
        ),
    ] {
        assert_matches_oracle(
            &db,
            sql,
            &format!("aggregate_in_scalar_subquery_over_empty_result_{name}"),
        );
    }
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

/// #289: a correlated subquery nested inside a FROM-subquery's own
/// SELECT list — `src/codegen/subquery.rs`'s `materialize_from_subquery`
/// now threads the full outer catalog through the FROM-subquery's own
/// scan (rather than just its own schema), so a subquery nested inside
/// it can resolve a reference to any catalog table, correlated (via the
/// scope-chain fallback, `other.id = t.id + 99`) or otherwise.
#[test]
fn correlated_subquery_inside_from_subquery_select_list_matches_oracle() {
    let db = subquery_fixture_db("from_subquery_correlated_select_list");
    assert_matches_oracle(
        &db,
        "SELECT * FROM (SELECT id, (SELECT a_id FROM other WHERE other.id = t.id + 99) AS sub \
         FROM t) AS s",
        "correlated_subquery_inside_from_subquery_select_list_matches_oracle",
    );
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

// #314: correlated scalar subquery memoized per distinct value of the
// single outer column it's correlated against (ADR-0021's follow-up
// from #303/#306). `catalog`'s `category` column repeats only 4
// distinct values across 20 rows — well under the cache's cap — so
// every repeat after the first should hit the cache instead of
// re-scanning `lookup`.

/// A fixture with a low-cardinality correlated column: `catalog.category`
/// cycles through 0..3 across many rows, correlated against
/// `lookup.cat`'s matching `val`.
fn memoized_correlated_fixture_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    let ddls = [
        "CREATE TABLE catalog(id INTEGER PRIMARY KEY, category INTEGER)",
        "CREATE TABLE lookup(cat INTEGER PRIMARY KEY, val INTEGER)",
    ];
    let catalog_values: Vec<String> = (0..20).map(|i| format!("({}, {})", i + 1, i % 4)).collect();
    let rows = [
        format!("INSERT INTO catalog VALUES {}", catalog_values.join(", ")),
        "INSERT INTO lookup VALUES (0, 5), (1, 10), (2, 100), (3, 1)".to_string(),
    ];
    if let Some(oracle) = pinned_oracle() {
        for stmt in ddls.iter().copied().chain(rows.iter().map(String::as_str)) {
            let status = Command::new(&oracle).arg(&db).arg(stmt).status().unwrap();
            assert!(status.success(), "oracle setup failed: {stmt}");
        }
    } else {
        assert!(run_exec(&db, "CREATE TABLE seed_bootstrap(x)")
            .status
            .success());
        for stmt in ddls.iter().copied().chain(rows.iter().map(String::as_str)) {
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

/// Correctness with repeated correlated values: every one of the 20
/// rows shares its `category` with 4 others, so the memoization cache
/// (if buggy) has plenty of opportunity to return a stale/wrong cached
/// result for a repeat.
#[test]
fn memoized_correlated_subquery_with_repeated_values_matches_oracle() {
    let db = memoized_correlated_fixture_db("memo_repeated");
    assert_matches_oracle(
        &db,
        "SELECT id, category FROM catalog \
         WHERE id > (SELECT val FROM lookup WHERE cat = catalog.category) ORDER BY id",
        "memoized_correlated_subquery_with_repeated_values_matches_oracle",
    );
}

/// A `NULL` correlated value must never hit or populate the cache
/// (SQL's `NULL = NULL` is unknown) — verified by mixing a `NULL`
/// `category` row in among the repeated non-NULL ones and confirming
/// the oracle-matching result still holds (the `NULL` row's own
/// subquery correlates on `cat = NULL`, always empty, so `id > (…)`
/// is NULL — excluded — same as the oracle).
#[test]
fn memoized_correlated_subquery_with_null_correlated_value_matches_oracle() {
    let db = memoized_correlated_fixture_db("memo_null");
    let insert_null = "INSERT INTO catalog VALUES (21, NULL)";
    if let Some(oracle) = pinned_oracle() {
        let status = Command::new(&oracle)
            .arg(&db)
            .arg(insert_null)
            .status()
            .unwrap();
        assert!(status.success());
    } else {
        let output = run_exec(&db, insert_null);
        assert!(output.status.success());
    }
    assert_matches_oracle(
        &db,
        "SELECT id, category FROM catalog \
         WHERE id > (SELECT val FROM lookup WHERE cat = catalog.category) ORDER BY id",
        "memoized_correlated_subquery_with_null_correlated_value_matches_oracle",
    );
}

/// #314 regression: a correlated, single-outer-column scalar subquery
/// in a single-table `WHERE` clause must set up its memoization cache
/// (`OpenEphemeral`, table mode) once, before the outer scan's
/// `Rewind` — not on every outer row. Same `-explain`-based shape as
/// the #306 hoist tests, but for the correlated (memoized, not hoisted)
/// case: unlike #306's hoist, the subquery's own `OpenRead` is *not*
/// expected before the outer `Rewind` (it only actually runs on a cache
/// miss, inside the loop) — only the cache table's `OpenEphemeral` is.
#[test]
fn correlated_subquery_memoization_cache_opens_before_outer_rewind() {
    let db = memoized_correlated_fixture_db("memo_explain");
    let program = explain(
        &db,
        "SELECT id FROM catalog WHERE id > (SELECT val FROM lookup WHERE cat = catalog.category)",
    );
    let eph_addr =
        first_opcode_line(&program, "OpenEphemeral").expect("expected an OpenEphemeral opcode");
    let rewind_addr = outer_rewind_line(&program);
    assert!(
        eph_addr < rewind_addr,
        "expected the memoization cache's OpenEphemeral (addr {eph_addr}) to be set up before \
         the outer scan's Rewind (addr {rewind_addr}); program:\n{program}"
    );
}

// Coverage: `collect_correlated_column` (memoize.rs) walks every
// `ExprKind` variant looking for the single outer column a memoizable
// subquery is correlated against; the tests above only exercise the
// plain `cat = catalog.category` shape. Each test below wraps the
// correlated reference in one more expression shape while keeping the
// candidate still single-outer-column (so #314's memoization still
// applies), matching `collect_correlated_column`'s own match arms.

/// A correlated reference wrapped in a unary `+` — covers
/// `collect_correlated_column`'s shared `Unary | IsNull | Cast |
/// Collate | Paren` arm.
#[test]
fn memoized_correlated_subquery_with_unary_wrapped_reference_matches_oracle() {
    let db = memoized_correlated_fixture_db("memo_unary");
    assert_matches_oracle(
        &db,
        "SELECT id, category FROM catalog \
         WHERE id > (SELECT val FROM lookup WHERE cat = +catalog.category) ORDER BY id",
        "memoized_correlated_subquery_with_unary_wrapped_reference_matches_oracle",
    );
}

/// A correlated reference on both sides of a `BETWEEN` — covers
/// `collect_correlated_column`'s `Between` arm, and revisits the
/// already-`found` column (the `Some(existing) if existing == name`
/// branch).
#[test]
fn memoized_correlated_subquery_with_between_matches_oracle() {
    let db = memoized_correlated_fixture_db("memo_between");
    assert_matches_oracle(
        &db,
        "SELECT id, category FROM catalog \
         WHERE id > (SELECT val FROM lookup \
         WHERE cat BETWEEN catalog.category AND catalog.category) ORDER BY id",
        "memoized_correlated_subquery_with_between_matches_oracle",
    );
}

/// A correlated reference inside an `IN (...)` list — covers
/// `collect_correlated_column`'s `In` arm.
#[test]
fn memoized_correlated_subquery_with_in_list_matches_oracle() {
    let db = memoized_correlated_fixture_db("memo_in_list");
    assert_matches_oracle(
        &db,
        "SELECT id, category FROM catalog \
         WHERE id > (SELECT val FROM lookup WHERE cat IN (catalog.category)) ORDER BY id",
        "memoized_correlated_subquery_with_in_list_matches_oracle",
    );
}

/// A correlated reference inside a `LIKE ... ESCAPE` — covers
/// `collect_correlated_column`'s `Like` arm, including the `escape`
/// operand branch.
#[test]
fn memoized_correlated_subquery_with_like_escape_matches_oracle() {
    let db = memoized_correlated_fixture_db("memo_like");
    assert_matches_oracle(
        &db,
        "SELECT id, category FROM catalog \
         WHERE id > (SELECT val FROM lookup \
         WHERE CAST(cat AS TEXT) LIKE CAST(catalog.category AS TEXT) ESCAPE '\\') ORDER BY id",
        "memoized_correlated_subquery_with_like_escape_matches_oracle",
    );
}

/// A correlated reference as a `CASE` operand — covers
/// `collect_correlated_column`'s `Case` arm, including the `operand`
/// and `else_` branches.
#[test]
fn memoized_correlated_subquery_with_case_operand_matches_oracle() {
    let db = memoized_correlated_fixture_db("memo_case");
    assert_matches_oracle(
        &db,
        "SELECT id, category FROM catalog \
         WHERE id > (SELECT val FROM lookup \
         WHERE cat = CASE catalog.category WHEN 0 THEN 0 WHEN 1 THEN 1 WHEN 2 THEN 2 ELSE 3 END) \
         ORDER BY id",
        "memoized_correlated_subquery_with_case_operand_matches_oracle",
    );
}

/// A correlated reference inside a function call's argument list —
/// covers `collect_correlated_column`'s `FunctionCall` arm.
#[test]
fn memoized_correlated_subquery_with_function_call_arg_matches_oracle() {
    let db = memoized_correlated_fixture_db("memo_func");
    assert_matches_oracle(
        &db,
        "SELECT id, category FROM catalog \
         WHERE id > (SELECT val FROM lookup WHERE cat = abs(catalog.category)) ORDER BY id",
        "memoized_correlated_subquery_with_function_call_arg_matches_oracle",
    );
}

/// A subquery correlated against *two* distinct outer columns is not
/// memoizable (the cache only has room for one probe value) — covers
/// `collect_correlated_column`'s second-distinct-outer-column
/// `ambiguous` branch. Falls back to the ordinary (unmemoized)
/// per-row correlated path, same result either way.
#[test]
fn correlated_subquery_with_two_outer_columns_is_not_memoized_matches_oracle() {
    let db = memoized_correlated_fixture_db("memo_two_outer_cols");
    assert_matches_oracle(
        &db,
        "SELECT id, category FROM catalog \
         WHERE id > (SELECT val FROM lookup WHERE cat = catalog.category AND val <> catalog.id) \
         ORDER BY id",
        "correlated_subquery_with_two_outer_columns_is_not_memoized_matches_oracle",
    );
}

/// A subquery containing a nested subquery-bearing expression
/// (`IN (SELECT ...)`) alongside its correlated reference is
/// conservatively not memoizable — covers `collect_correlated_column`'s
/// nested-subquery `ambiguous` branch.
#[test]
fn correlated_subquery_with_nested_subquery_is_not_memoized_matches_oracle() {
    let db = memoized_correlated_fixture_db("memo_nested_subquery");
    assert_matches_oracle(
        &db,
        "SELECT id, category FROM catalog \
         WHERE id > (SELECT val FROM lookup \
         WHERE cat = catalog.category AND val IN (SELECT val FROM lookup)) ORDER BY id",
        "correlated_subquery_with_nested_subquery_is_not_memoized_matches_oracle",
    );
}

// Coverage: `correlation.rs`'s `walk_expr_for_correlation` (the #306
// hoist's correlation check) walks the same `ExprKind` shapes, but for
// a WHERE-clause scalar subquery — as opposed to memoize.rs's
// `collect_correlated_column` above, which only fires once a subquery
// is already known to be correlated. These use `t`/`other` (not the
// memoization fixture) since #306's hoist, unlike #314's memoize,
// doesn't need a low-cardinality correlated column.

/// An uncorrelated scalar subquery whose own WHERE clause exercises
/// `walk_expr_for_correlation`'s `Between`/`In`/`Like`/`Case`/`Unary`/
/// `IsNull`/`Paren`/`Collate`/`FunctionCall` arms, all referencing only
/// the subquery's own column — none of them should trip `correlated`,
/// so the subquery stays hoistable (#306) and this doubles as coverage
/// for `hoist_uncorrelated_where_subqueries`'s success path.
#[test]
fn hoistable_scalar_subquery_with_every_expr_shape_matches_oracle() {
    let db = subquery_fixture_db("hoist_expr_shapes");
    assert_matches_oracle(
        &db,
        "SELECT id, x FROM t WHERE x < (SELECT other.id FROM other \
         WHERE other.a_id BETWEEN 1 AND 100 \
         AND other.a_id IN (1, 2, 3) \
         AND CAST(other.a_id AS TEXT) LIKE '1%' ESCAPE '\\' \
         AND CASE WHEN other.a_id = 1 THEN 1 ELSE 0 END = 1 \
         AND -other.a_id < 0 \
         AND other.a_id IS NOT NULL \
         AND (other.a_id) = other.a_id \
         AND other.a_id COLLATE NOCASE = other.a_id \
         AND abs(other.a_id) >= 0)",
        "hoistable_scalar_subquery_with_every_expr_shape_matches_oracle",
    );
}

/// A scalar subquery whose own WHERE clause is a nested `EXISTS (...)`
/// — conservatively correlated regardless of whether the nested
/// subquery itself references the outer scope — covers
/// `walk_expr_for_correlation`'s nested-subquery-bearing arm, and
/// exercises `try_hoist_conjunct`'s "not hoistable" fallback for a
/// WHERE-clause scalar subquery (#306's success path is covered
/// elsewhere; this is the graceful non-hoist path for the same
/// `Binary`-comparison shape).
#[test]
fn correlated_via_nested_exists_scalar_subquery_is_not_hoisted_matches_oracle() {
    let db = subquery_fixture_db("hoist_nested_exists");
    assert_matches_oracle(
        &db,
        "SELECT id, x FROM t WHERE x < (SELECT other.id FROM other WHERE EXISTS (SELECT 1 FROM other))",
        "correlated_via_nested_exists_scalar_subquery_is_not_hoisted_matches_oracle",
    );
}
