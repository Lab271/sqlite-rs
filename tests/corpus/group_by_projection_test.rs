//! #506 regression coverage: `compile_grouped_scan`'s pass 1 now only
//! serializes schema columns actually referenced by the `GROUP BY` key,
//! aggregate arguments, or plain result/`HAVING` columns
//! (`columns_needed_for_projection`) — every other column becomes a
//! `Null` placeholder in the sort record instead of a real per-row
//! read. These tests exercise the shape the pruning has to get right:
//! a `SELECT` that reads a plain (non-key, non-aggregate) column
//! alongside the `GROUP BY` key and an aggregate, over a table with
//! extra columns the query never touches at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-group-by-projection-{label}-{}-{n}",
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
    let ours = run_query(db, sql);
    if let Some(oracle) = pinned_oracle() {
        let theirs = oracle_select(&oracle, db, sql);
        assert_eq!(ours, theirs, "mismatch for {sql:?}");
    } else {
        skip_no_oracle(test_name);
    }
}

/// Five columns (`id, n, x, f, s`), mirroring the `group_by_agg` perf
/// benchmark's fixture shape (#506's own repro) — a query that only
/// needs `bucket`/`x` should still get correct results for every other
/// column's worth of data sitting unread in the same rows, and a query
/// that *does* read one of those otherwise-unreferenced columns must
/// still see its real value, not the `Null` placeholder pass 1 uses for
/// columns nothing asks for.
fn fixture_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    let ddl = "CREATE TABLE bench_data(id INTEGER PRIMARY KEY, bucket INTEGER, x INTEGER, f REAL, s TEXT)";
    let rows = [
        "INSERT INTO bench_data VALUES (1, 1, 10, 1.5, 'a')",
        "INSERT INTO bench_data VALUES (2, 1, 20, 2.5, 'b')",
        "INSERT INTO bench_data VALUES (3, 2, 30, 3.5, 'c')",
        "INSERT INTO bench_data VALUES (4, 2, 40, 4.5, 'd')",
        "INSERT INTO bench_data VALUES (5, 3, 50, 5.5, 'e')",
    ];
    if let Some(oracle) = pinned_oracle() {
        for stmt in std::iter::once(ddl).chain(rows.iter().copied()) {
            let status = Command::new(&oracle).arg(&db).arg(stmt).status().unwrap();
            assert!(status.success(), "oracle setup failed: {stmt}");
        }
    } else {
        assert!(run_exec(&db, ddl).status.success());
        for stmt in rows {
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

/// The query the pruning was built for: `bucket` (the `GROUP BY` key)
/// and `x` (`SUM`'s argument) are read; `id`, `f`, `s` are never
/// referenced at all, so pass 1 should skip real reads of them.
#[test]
fn group_by_agg_only_needs_key_and_aggregate_arg() {
    let db = fixture_db("key_and_arg");
    assert_matches_oracle(
        &db,
        "SELECT bucket, COUNT(*), SUM(x) FROM bench_data GROUP BY bucket",
        "group_by_agg_only_needs_key_and_aggregate_arg",
    );
}

/// #506's acceptance criteria: a plain (non-key, non-aggregate) result
/// column (`s`) must still read its real per-group value correctly —
/// this is exactly the "arbitrary row" snapshot `read_row_columns_into`
/// takes, which must NOT have been pruned to `Null` just because it
/// isn't the `GROUP BY` key or an aggregate argument.
#[test]
fn group_by_agg_with_plain_non_key_non_aggregate_result_column() {
    let db = fixture_db("plain_column");
    assert_matches_oracle(
        &db,
        "SELECT bucket, s, SUM(x) FROM bench_data GROUP BY bucket",
        "group_by_agg_with_plain_non_key_non_aggregate_result_column",
    );
}

/// Same shape again, but the plain column (`f`) is read only via
/// `HAVING`, not the `SELECT` list — a second, independent site that
/// must be included in the referenced-column set.
#[test]
fn group_by_agg_having_references_a_non_key_non_aggregate_column() {
    let db = fixture_db("having_column");
    assert_matches_oracle(
        &db,
        "SELECT bucket, SUM(x) FROM bench_data GROUP BY bucket HAVING MAX(f) > 2.0",
        "group_by_agg_having_references_a_non_key_non_aggregate_column",
    );
}

/// `SELECT *` must fall back to every column (the conservative bail in
/// `columns_needed_for_projection`) rather than only the columns a
/// narrower analysis might otherwise infer.
#[test]
fn group_by_agg_star_projection_still_sees_every_column() {
    let db = fixture_db("star");
    assert_matches_oracle(
        &db,
        "SELECT bucket, id, f, s, SUM(x) FROM bench_data GROUP BY bucket",
        "group_by_agg_star_projection_still_sees_every_column",
    );
}
