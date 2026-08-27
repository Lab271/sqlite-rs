// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
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

/// The `-explain` bytecode listing for `db`/`sql` -- used below to
/// check for `FilterAdd`/`Filter` opcodes (#464), which `EXPLAIN QUERY
/// PLAN`'s human-readable summary doesn't surface.
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

/// #470 (spec 011): without `ANALYZE`, a plain `INNER JOIN` between two
/// full-scan tables still compiles FROM-clause order unchanged (t1
/// outermost) -- the #461 "stats-free behavior is unaffected" guarantee
/// extended to join order.
#[test]
fn join_order_unchanged_without_analyze() {
    let Some(db) = seed_db("join-order-no-stats") else {
        return skip_no_oracle("join_order_unchanged_without_analyze");
    };
    exec_ok(&db, "CREATE TABLE t1(a INTEGER)");
    exec_ok(&db, "CREATE TABLE t2(x INTEGER)");
    exec_ok(&db, "INSERT INTO t1 VALUES (1), (2), (3), (4), (5)");
    exec_ok(&db, "INSERT INTO t2 VALUES (1)");

    let plan = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM t1 JOIN t2 ON t1.a = t2.x",
    );
    assert_eq!(plan, "0|0|0|SCAN t1\n1|0|0|SCAN t2\n", "got: {plan}");
}

/// #470/#462 (spec 011): once `ANALYZE` has recorded that `t1` has far
/// more rows than `t2`, a plain `INNER JOIN` between the two full-scan
/// tables is reordered to scan the smaller table (`t2`) outermost,
/// instead of always compiling FROM-clause order -- and the join still
/// returns the same rows as the un-reordered plan (oracle-equivalent
/// result set, only the scan order changes).
#[test]
fn join_order_reorders_by_analyze_row_counts() {
    let Some(db) = seed_db("join-order-analyze") else {
        return skip_no_oracle("join_order_reorders_by_analyze_row_counts");
    };
    exec_ok(&db, "CREATE TABLE t1(a INTEGER)");
    exec_ok(&db, "CREATE TABLE t2(x INTEGER)");
    exec_ok(&db, "INSERT INTO t1 VALUES (1), (2), (3), (4), (5)");
    exec_ok(&db, "INSERT INTO t2 VALUES (1)");
    exec_ok(&db, "ANALYZE");

    let plan = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM t1 JOIN t2 ON t1.a = t2.x",
    );
    assert_eq!(plan, "0|0|0|SCAN t2\n1|0|0|SCAN t1\n", "got: {plan}");

    let rows = run_query(
        &db,
        "SELECT t1.a, t2.x FROM t1 JOIN t2 ON t1.a = t2.x ORDER BY t1.a",
    );
    assert_eq!(rows, "1|1\n", "got: {rows}");
}

/// #510: a join whose `ON` equality reaches the *smaller* table via its
/// `INTEGER PRIMARY KEY` (rowid alias) must still be ordered with that
/// table innermost -- i.e. `join_order::seekable_tables`'s bias beats
/// `ANALYZE`'s raw row-count ordering, since a rowid seek is O(1)
/// regardless of the table's own size and is always cheaper as an inner
/// probe than as the outer scan.
#[test]
fn join_order_prefers_seekable_inner_over_smaller_outer() {
    let Some(db) = seed_db("join-order-seekable") else {
        return skip_no_oracle("join_order_prefers_seekable_inner_over_smaller_outer");
    };
    exec_ok(
        &db,
        "CREATE TABLE bench_lookup(code INTEGER PRIMARY KEY, label TEXT)",
    );
    exec_ok(
        &db,
        "CREATE TABLE bench_data(id INTEGER PRIMARY KEY, bucket INTEGER)",
    );
    exec_ok(&db, "INSERT INTO bench_lookup VALUES (1, 'a'), (2, 'b')");
    exec_ok(
        &db,
        "INSERT INTO bench_data VALUES (1, 1), (2, 2), (3, 1), (4, 2), (5, 1)",
    );
    exec_ok(&db, "ANALYZE");

    let plan = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT bench_data.id, bench_lookup.label \
         FROM bench_data JOIN bench_lookup ON bench_data.bucket = bench_lookup.code",
    );
    assert_eq!(
        plan,
        "0|0|0|SCAN bench_data\n1|0|0|SEARCH bench_lookup USING INTEGER PRIMARY KEY (rowid=?)\n",
        "got: {plan}"
    );

    let rows = run_query(
        &db,
        "SELECT bench_data.id, bench_lookup.label \
         FROM bench_data JOIN bench_lookup ON bench_data.bucket = bench_lookup.code \
         ORDER BY bench_data.id",
    );
    assert_eq!(rows, "1|a\n2|b\n3|a\n4|b\n5|a\n", "got: {rows}");
}

/// #464 (spec 011): once `ANALYZE` shows a join level's table has
/// enough rows and no rowid/unique-index seek is structurally
/// available, the compiled program prefaces that level's nested-loop
/// scan -- since #545, a transient automatic index (`OpenEphemeral` +
/// `AutoIndexSeek`) rather than a `FilterAdd`/`Filter` Bloom pre-pass,
/// since an automatic index makes both hits and misses cheap and so is
/// tried first (see `join_access::choose_auto_index_probe`) -- and the
/// join still returns the same rows as the oracle.
#[test]
fn automatic_index_prefaces_unindexed_join_level_once_analyzed() {
    let Some(db) = seed_db("auto-index") else {
        return skip_no_oracle("automatic_index_prefaces_unindexed_join_level_once_analyzed");
    };
    exec_ok(&db, "CREATE TABLE t1(a INTEGER)");
    exec_ok(&db, "CREATE TABLE t2(x INTEGER)");
    let values: Vec<String> = (1..=40).map(|n| format!("({n})")).collect();
    exec_ok(&db, &format!("INSERT INTO t1 VALUES {}", values.join(", ")));
    exec_ok(&db, "INSERT INTO t2 VALUES (5), (37)");
    exec_ok(&db, "ANALYZE");

    let plan = explain(&db, "SELECT * FROM t1 JOIN t2 ON t1.a = t2.x");
    assert!(plan.contains("OpenEphemeral"), "got: {plan}");
    assert!(plan.contains("AutoIndexSeek"), "got: {plan}");
    assert!(!plan.contains("FilterAdd"), "got: {plan}");

    let rows = run_query(
        &db,
        "SELECT t1.a, t2.x FROM t1 JOIN t2 ON t1.a = t2.x ORDER BY t1.a",
    );
    assert_eq!(rows, "5|5\n37|37\n", "got: {rows}");
    assert_matches_analyze_oracle(
        &db,
        "SELECT t1.a, t2.x FROM t1 JOIN t2 ON t1.a = t2.x ORDER BY t1.a",
    );
}

/// #545: a transient automatic index also handles duplicate join-key
/// values on the outer probe side correctly -- every outer row sharing
/// a key must still find every matching inner row, exercising the
/// `SeekIndexEq` + recheck-then-`IdxNext` walk-while-still-equal loop
/// beyond its first match.
#[test]
fn automatic_index_handles_duplicate_join_keys() {
    let Some(db) = seed_db("auto-index-dupes") else {
        return skip_no_oracle("automatic_index_handles_duplicate_join_keys");
    };
    exec_ok(&db, "CREATE TABLE t1(a INTEGER)");
    exec_ok(&db, "CREATE TABLE t2(x INTEGER)");
    let values: Vec<String> = (1..=40).map(|n| format!("({n})")).collect();
    exec_ok(&db, &format!("INSERT INTO t1 VALUES {}", values.join(", ")));
    // t1 now has three rows valued 5 (one from the 1..=40 range, two
    // explicit) and one valued 37; t2 has two rows valued 5 and one
    // valued 37 -- 3*2 + 1*1 = 7 matching pairs total.
    exec_ok(&db, "INSERT INTO t1 VALUES (5), (5)");
    exec_ok(&db, "INSERT INTO t2 VALUES (5), (5), (37)");
    exec_ok(&db, "ANALYZE");

    let rows = run_query(
        &db,
        "SELECT t1.a, t2.x FROM t1 JOIN t2 ON t1.a = t2.x ORDER BY t1.a, t2.x",
    );
    assert_matches_analyze_oracle(
        &db,
        "SELECT t1.a, t2.x FROM t1 JOIN t2 ON t1.a = t2.x ORDER BY t1.a, t2.x",
    );
    assert_eq!(rows.lines().count(), 3 * 2 + 1, "got: {rows}");
}

/// #547: `EXPLAIN QUERY PLAN` must actually report the transient
/// automatic index #545/#464 build when it fires, matching real
/// sqlite3's own wording (`SEARCH t USING AUTOMATIC COVERING INDEX
/// (col=?)`, confirmed empirically, sqlite3 3.51.0) — before this fix,
/// `explain_query_plan` never consulted `choose_auto_index_probe` at
/// all, so this level silently fell through to a blanket `SCAN` in the
/// human-readable plan even while the compiled program (per
/// `automatic_index_prefaces_unindexed_join_level_once_analyzed` above)
/// genuinely built and probed the index at runtime.
#[test]
fn eqp_reports_automatic_index_once_analyzed() {
    let Some(db) = seed_db("auto-index-eqp") else {
        return skip_no_oracle("eqp_reports_automatic_index_once_analyzed");
    };
    exec_ok(&db, "CREATE TABLE t1(a INTEGER)");
    exec_ok(&db, "CREATE TABLE t2(x INTEGER)");
    let values: Vec<String> = (1..=40).map(|n| format!("({n})")).collect();
    exec_ok(&db, &format!("INSERT INTO t1 VALUES {}", values.join(", ")));
    exec_ok(&db, "INSERT INTO t2 VALUES (5), (37)");
    exec_ok(&db, "ANALYZE");

    let plan = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM t1 JOIN t2 ON t1.a = t2.x",
    );
    // t2 (2 rows) sorts outermost by `join_order.rs`'s own "smallest
    // table outermost" heuristic, so t1 (40 rows) is the inner level
    // whose scan this level's automatic index replaces.
    assert!(
        plan.contains("SEARCH t1 USING AUTOMATIC COVERING INDEX (a=?)"),
        "got: {plan}"
    );
    assert!(!plan.contains("SCAN t1"), "got: {plan}");
}

/// #464 (spec 011): below [`join_access::MIN_ROWS_TO_BLOOM`]'s
/// threshold (or without `ANALYZE` at all), no `FilterAdd`/`Filter`
/// opcode is emitted -- the pre-pass's overhead isn't worth it for a
/// small table, and a stats-free database is byte-for-byte unaffected.
#[test]
fn bloom_filter_is_skipped_below_row_threshold() {
    let Some(db) = seed_db("bloom-filter-small") else {
        return skip_no_oracle("bloom_filter_is_skipped_below_row_threshold");
    };
    exec_ok(&db, "CREATE TABLE t1(a INTEGER)");
    exec_ok(&db, "CREATE TABLE t2(x INTEGER)");
    exec_ok(&db, "INSERT INTO t1 VALUES (1), (2), (3)");
    exec_ok(&db, "INSERT INTO t2 VALUES (2)");
    exec_ok(&db, "ANALYZE");

    let plan = explain(&db, "SELECT * FROM t1 JOIN t2 ON t1.a = t2.x");
    assert!(!plan.contains("FilterAdd"), "got: {plan}");
}

/// Verifies `sql`'s rows against the pinned oracle after `ANALYZE` has
/// run against `db` -- same shape as `join_test.rs`'s
/// `assert_matches_oracle`, but reusable here without pulling in that
/// module's own fixture-building helpers.
fn assert_matches_analyze_oracle(db: &Path, sql: &str) {
    let Some(oracle) = pinned_oracle() else {
        return;
    };
    let oracle_output = Command::new(&oracle)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running oracle {} {sql:?}: {e}", db.display()));
    assert!(oracle_output.status.success());
    let expected = String::from_utf8_lossy(&oracle_output.stdout).into_owned();
    let actual = run_query(db, sql);
    assert_eq!(actual, expected, "sqlite-rs vs oracle for {sql:?}");
}
