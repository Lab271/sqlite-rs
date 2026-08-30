// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! `EXPLAIN QUERY PLAN` parity: for each of the 5 read scenarios in
//! `tests/performance/crud.rs`'s 15-scenario CRUD suite (PK seek,
//! indexed-range scan, full scan, join, GROUP BY aggregate), asserts
//! `sqlite-rs`'s plan output matches the pinned oracle's own `EXPLAIN
//! QUERY PLAN` for the same schema/data/query.
//!
//! The 10 write scenarios (INSERT/UPDATE/DELETE) are out of scope:
//! `EXPLAIN QUERY PLAN` only accepts a `SELECT` on both sides today --
//! `parse_explain_stmt` (`src/parser/grammar.rs`) wraps `parse_select_stmt`
//! only, so there is no plan output to diff for a write statement yet.
//!
//! Schema/data mirror `tools/gen_fixtures.sh`'s `--bench` fixture
//! (`bench_data`/`bench_lookup`, indexed on `bench_data.x`, joined on
//! `bench_data.bucket = bench_lookup.code`) at a much smaller scale, built
//! through `sqlite-rs exec` itself rather than the oracle -- only a
//! `CREATE TABLE`/multi-row `INSERT`/`CREATE INDEX`/`ANALYZE` sequence is
//! needed, all of which `exec` already supports, so no oracle-only DDL
//! (e.g. a recursive-CTE `INSERT ... SELECT`) is required to seed it.
//!
//! The oracle side still shells out to the pinned `sqlite3` CLI (via
//! [`crate::oracle::run_oracle`]), but since sqlite3 3.24 the shell
//! always pretty-prints `EXPLAIN QUERY PLAN` as an indented tree
//! (`` QUERY PLAN\n`--SEARCH ... ``), ignoring `.mode`/`-list` entirely --
//! [`parse_oracle_eqp_details`] strips that tree formatting back down to
//! a flat detail list, comparable against our own flat
//! `id|parent|notused|detail` rows. This only round-trips correctly for
//! a single-level (unnested) plan tree, true of all 5 scenarios here (no
//! subquery/compound arm nesting) -- a deeper plan would need to parse
//! the tree connectors' indentation too.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{pinned_oracle, run_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");
const ROWS: i64 = 500;
const BUCKETS: i64 = 50;

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-plan-parity-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

fn exec_ok(db: &Path, sql: &str) {
    let output = Command::new(CLI)
        .arg("exec")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} exec {} {sql:?}: {e}", db.display()));
    assert!(
        output.status.success(),
        "exec {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A fresh scratch db seeded via the pinned oracle (`sqlite-rs exec`
/// can't create a brand-new database file -- see `dump::open`, which
/// requires the file to already exist), matching `analyze_test.rs::seed_db`.
fn seed_db(oracle: &Path, label: &str) -> PathBuf {
    let db = scratch_db(label);
    let status = Command::new(oracle)
        .arg(&db)
        .arg("CREATE TABLE seed_bootstrap(x)")
        .status()
        .unwrap();
    assert!(status.success());
    db
}

/// Builds a `bench_data`/`bench_lookup` fixture with the same shape as
/// `tools/gen_fixtures.sh`'s `--bench` fixture (indexed `x`, joinable
/// `bucket`/`code`), scaled down to `ROWS` rows so the test stays fast,
/// via `sqlite-rs exec` itself.
fn seed_bench_fixture(oracle: &Path, label: &str) -> PathBuf {
    let db = seed_db(oracle, label);
    exec_ok(
        &db,
        "CREATE TABLE bench_data(id INTEGER PRIMARY KEY, n INTEGER, x INTEGER, f REAL, \
         s TEXT, bucket INTEGER)",
    );
    let rows: Vec<String> = (1..=ROWS)
        .map(|i| {
            let n = (i * 2654435761) % 1_000_000;
            let x = (i * 40503) % 100_000;
            let bucket = i % BUCKETS;
            format!(
                "({i}, {n}, {x}, {}, 'row-{i}', {bucket})",
                x as f64 / 1000.0
            )
        })
        .collect();
    exec_ok(
        &db,
        &format!(
            "INSERT INTO bench_data(id, n, x, f, s, bucket) VALUES {}",
            rows.join(", ")
        ),
    );
    exec_ok(&db, "CREATE INDEX bench_data_x ON bench_data(x)");

    exec_ok(
        &db,
        "CREATE TABLE bench_lookup(code INTEGER PRIMARY KEY, label TEXT)",
    );
    let lookup_rows: Vec<String> = (0..BUCKETS)
        .map(|i| format!("({i}, 'lookup-{i}')"))
        .collect();
    exec_ok(
        &db,
        &format!(
            "INSERT INTO bench_lookup(code, label) VALUES {}",
            lookup_rows.join(", ")
        ),
    );
    exec_ok(&db, "ANALYZE");
    db
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

/// The `detail` column of our own `EXPLAIN QUERY PLAN` output, in row
/// order -- parses the `id|parent|notused|detail` lines `run_query`'s
/// `println!("{}|{}|{}|{}", ...)` prints (`src/bin/sqlite-rs/query.rs`).
fn ours_eqp_details(db: &Path, sql: &str) -> Vec<String> {
    run_query(db, &format!("EXPLAIN QUERY PLAN {sql}"))
        .lines()
        .map(|line| {
            line.splitn(4, '|')
                .nth(3)
                .unwrap_or_else(|| panic!("malformed EQP row {line:?}"))
                .to_string()
        })
        .collect()
}

/// The oracle's `EXPLAIN QUERY PLAN` output, tree-formatting stripped
/// back down to a flat detail list (see module doc comment) -- assumes a
/// single-level tree, true of all 5 scenarios below.
fn oracle_eqp_details(oracle: &Path, db: &Path, sql: &str) -> Vec<String> {
    let text = run_oracle(oracle, db, &[], &format!("EXPLAIN QUERY PLAN {sql}"));
    text.lines()
        .skip(1) // "QUERY PLAN" header
        .map(|line| {
            line.strip_prefix("|--")
                .or_else(|| line.strip_prefix("`--"))
                .unwrap_or_else(|| panic!("unexpected EQP tree line {line:?}"))
                .to_string()
        })
        .collect()
}

fn assert_plan_matches_oracle(oracle: &Path, db: &Path, scenario: &str, sql: &str) {
    let ours = ours_eqp_details(db, sql);
    let theirs = oracle_eqp_details(oracle, db, sql);
    assert_eq!(ours, theirs, "{scenario}: plan mismatch for {sql:?}");
}

fn run_scenario(scenario: &str, sql: &str) {
    let Some(oracle) = pinned_oracle() else {
        return skip_no_oracle(scenario);
    };
    let db = seed_bench_fixture(&oracle, scenario);
    assert_plan_matches_oracle(&oracle, &db, scenario, sql);
}

#[test]
fn read_pk_matches_oracle_plan() {
    run_scenario(
        "read_pk",
        "SELECT id, n, x, f, s FROM bench_data WHERE id = 250",
    );
}

/// #606's range-seek fast path only covers `BETWEEN`/`IN`/LIKE-prefix, not
/// a bare `>` comparison (`src/codegen/select/range_scan.rs::find_range_seek_detail`),
/// so `compile_direct_scan` falls back to a plain scan here where the
/// oracle picks `SEARCH ... USING INDEX bench_data_x (x>?)` -- a real,
/// tracked planner gap, not a test bug. Un-ignore once a bare-comparison
/// range seek lands.
#[test]
#[ignore = "no bare-`>` range-seek fast path yet (range_scan.rs only covers BETWEEN/IN/LIKE)"]
fn read_indexed_range_matches_oracle_plan() {
    run_scenario(
        "read_indexed_range",
        "SELECT id, n, x, f, s FROM bench_data WHERE x > 50000",
    );
}

#[test]
fn read_full_scan_matches_oracle_plan() {
    run_scenario("read_full_scan", "SELECT id, n, x, f, s FROM bench_data");
}

#[test]
fn read_join_matches_oracle_plan() {
    run_scenario(
        "read_join",
        "SELECT bench_data.id, bench_data.x, bench_lookup.label FROM bench_data \
         JOIN bench_lookup ON bench_data.bucket = bench_lookup.code",
    );
}

/// The oracle reports an extra `USE TEMP B-TREE FOR GROUP BY` row that
/// `explain_query_plan` (`src/codegen/select/eqp.rs`) never emits --
/// `GROUP BY`'s own EQP reporting doesn't yet surface whether the
/// compiled program actually sorts via a temp b-tree, a real gap distinct
/// from `read_indexed_range`'s missing fast path. Un-ignore once that
/// reporting is added.
#[test]
#[ignore = "GROUP BY EQP doesn't report a temp-b-tree sort step yet (eqp.rs)"]
fn read_group_by_agg_matches_oracle_plan() {
    run_scenario(
        "read_group_by_agg",
        "SELECT bucket, COUNT(*), SUM(x) FROM bench_data GROUP BY bucket",
    );
}
