//! End-to-end oracle-diff tests for INNER/LEFT [OUTER]/CROSS JOIN
//! (#237), via the `sqlite-rs` CLI's `exec`/`query` subcommands —
//! mirroring `cli_write_test.rs`'s scratch-db-per-test shape.

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("sqlite-rs-join-{label}-{}-{n}", std::process::id()));
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

/// A fresh scratch db seeded with every `ddl` statement given, then the
/// join fixture's rows. Same oracle-if-available, else-bootstrap-via-
/// `exec` shape as `cli_write_test.rs`'s `multi_table_db`.
fn join_fixture_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    let ddls = [
        "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT)",
        "CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER, tag TEXT)",
        "CREATE TABLE c(id INTEGER PRIMARY KEY, b_id INTEGER, note TEXT)",
    ];
    let rows = [
        "INSERT INTO a VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
        // a_id=1 matches a.id=1 twice; a_id=99 matches no row in `a`.
        "INSERT INTO b VALUES (10, 1, 'x'), (11, 1, 'y'), (12, 99, 'z')",
        "INSERT INTO c VALUES (100, 10, 'p'), (101, 11, 'q')",
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

/// Runs `sql` through the CLI and, when the pinned oracle is available,
/// asserts the two outputs are byte-identical; otherwise skips the
/// cross-check but still exercises the CLI path so a panic/crash is
/// still caught.
fn assert_matches_oracle(db: &Path, sql: &str, test_name: &str) {
    let ours = run_query(db, sql);
    if let Some(oracle) = pinned_oracle() {
        let theirs = oracle_select(&oracle, db, sql);
        assert_eq!(ours, theirs, "mismatch for {sql:?}");
    } else {
        skip_no_oracle(test_name);
    }
}

#[test]
fn inner_join_matches_oracle() {
    let db = join_fixture_db("inner");
    assert_matches_oracle(
        &db,
        "SELECT * FROM a JOIN b ON a.id = b.a_id",
        "inner_join_matches_oracle",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// Bare `JOIN` and `INNER JOIN` must compile identically.
#[test]
fn inner_and_bare_join_are_equivalent() {
    let db = join_fixture_db("inner_bare");
    let bare = run_query(&db, "SELECT * FROM a JOIN b ON a.id = b.a_id");
    let inner = run_query(&db, "SELECT * FROM a INNER JOIN b ON a.id = b.a_id");
    assert_eq!(bare, inner);
    assert_matches_oracle(
        &db,
        "SELECT * FROM a INNER JOIN b ON a.id = b.a_id",
        "inner_and_bare_join_are_equivalent",
    );
}

/// LEFT JOIN: `a`'s row `id=3` (carol) has no matching `b` row, so it
/// must appear exactly once with `b`'s columns NULL.
#[test]
fn left_join_null_extends_unmatched_rows() {
    let db = join_fixture_db("left");
    let output = run_query(
        &db,
        "SELECT a.id, a.name, b.id, b.tag FROM a LEFT JOIN b ON a.id = b.a_id",
    );
    assert_eq!(
        output, "1|alice|10|x\n1|alice|11|y\n2|bob||\n3|carol||\n",
        "unmatched left rows (bob, carol) must appear once with NULL b columns"
    );
    assert_matches_oracle(
        &db,
        "SELECT a.id, a.name, b.id, b.tag FROM a LEFT JOIN b ON a.id = b.a_id",
        "left_join_null_extends_unmatched_rows",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

#[test]
fn cross_join_is_the_full_cartesian_product() {
    let db = join_fixture_db("cross");
    let output = run_query(&db, "SELECT a.id, b.id FROM a CROSS JOIN b");
    assert_eq!(
        output.lines().count(),
        3 * 3,
        "3 rows in `a` x 3 rows in `b` = 9 rows; got: {output}"
    );
    assert_matches_oracle(
        &db,
        "SELECT a.id, b.id FROM a CROSS JOIN b",
        "cross_join_is_the_full_cartesian_product",
    );
}

/// A 3-way join chain: `a JOIN b ON ... JOIN c ON ...`.
#[test]
fn three_way_inner_join_matches_oracle() {
    let db = join_fixture_db("three_way");
    assert_matches_oracle(
        &db,
        "SELECT a.name, b.tag, c.note FROM a JOIN b ON a.id = b.a_id JOIN c ON b.id = c.b_id",
        "three_way_inner_join_matches_oracle",
    );
}

/// A WHERE clause after the join filters the joined result, not just
/// one side.
#[test]
fn where_clause_filters_the_joined_result() {
    let db = join_fixture_db("where_after_join");
    assert_matches_oracle(
        &db,
        "SELECT a.name, b.tag FROM a JOIN b ON a.id = b.a_id WHERE b.tag = 'y'",
        "where_clause_filters_the_joined_result",
    );
}

/// LIMIT applies to the joined output as a whole.
#[test]
fn limit_applies_to_the_joined_output() {
    let db = join_fixture_db("limit_after_join");
    let output = run_query(
        &db,
        "SELECT a.id, b.id FROM a JOIN b ON a.id = b.a_id LIMIT 1",
    );
    assert_eq!(output, "1|10\n");
}

/// `star`-expansion across a join projects every table's columns, in
/// FROM order.
#[test]
fn star_expands_across_every_joined_table() {
    let db = join_fixture_db("star");
    assert_matches_oracle(
        &db,
        "SELECT * FROM a JOIN b ON a.id = b.a_id",
        "star_expands_across_every_joined_table",
    );
}

/// #243: an inner table's `ON` equality against the *outer* table's
/// rowid (`a`'s `INTEGER PRIMARY KEY`) compiles to a `SeekRowid` point
/// lookup instead of a full `Rewind`/`Next` scan — result must be
/// identical to the oracle regardless of which side of `ON` names which
/// table, and regardless of table order in `FROM`.
#[test]
fn join_on_outer_rowid_uses_seek_and_matches_oracle() {
    let db = join_fixture_db("rowid_seek");
    assert_matches_oracle(
        &db,
        "SELECT * FROM b JOIN a ON b.a_id = a.id",
        "join_on_outer_rowid_uses_seek_and_matches_oracle",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// #243: `LEFT JOIN`'s null-extension still fires correctly when the
/// inner table's `ON` equality uses the `SeekRowid` fast path — `b`'s
/// `a_id=99` row has no matching `a.id`, so it must still appear once
/// with `a`'s columns NULL (not zero rows).
#[test]
fn left_join_on_rowid_seek_still_null_extends_unmatched_rows() {
    let db = join_fixture_db("left_rowid_seek");
    let output = run_query(
        &db,
        "SELECT b.id, b.a_id, a.id, a.name FROM b LEFT JOIN a ON b.a_id = a.id",
    );
    assert_eq!(
        output, "10|1|1|alice\n11|1|1|alice\n12|99||\n",
        "unmatched b row (a_id=99) must appear once with NULL a columns"
    );
    assert_matches_oracle(
        &db,
        "SELECT b.id, b.a_id, a.id, a.name FROM b LEFT JOIN a ON b.a_id = a.id",
        "left_join_on_rowid_seek_still_null_extends_unmatched_rows",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// #243: an inner table's `ON` equality against a `UNIQUE`-indexed
/// (non-rowid) column compiles to a `SeekIndexEq` + `IdxRowid` +
/// `SeekRowid` point lookup instead of a full scan.
#[test]
fn join_on_unique_indexed_column_uses_seek_and_matches_oracle() {
    let db = scratch_db("unique_index_seek");
    let ddls = [
        "CREATE TABLE d(id INTEGER PRIMARY KEY, code TEXT)",
        "CREATE UNIQUE INDEX idx_d_code ON d(code)",
        "CREATE TABLE e(id INTEGER PRIMARY KEY, d_code TEXT)",
    ];
    let rows = [
        "INSERT INTO d VALUES (1, 'x'), (2, 'y'), (3, 'z')",
        "INSERT INTO e VALUES (10, 'x'), (11, 'y'), (12, 'nomatch')",
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
    assert_matches_oracle(
        &db,
        "SELECT e.id, d.id, d.code FROM e JOIN d ON e.d_code = d.code",
        "join_on_unique_indexed_column_uses_seek_and_matches_oracle",
    );
    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    }
}

/// Still-unsupported constructs (USING, NATURAL, comma-join, RIGHT/
/// FULL) must fail cleanly as "unsupported", not panic or silently
/// mis-parse.
#[test]
fn still_unsupported_join_forms_fail_cleanly() {
    let db = join_fixture_db("unsupported");
    for sql in [
        "SELECT * FROM a JOIN b USING (id)",
        "SELECT * FROM a NATURAL JOIN b",
        "SELECT * FROM a, b",
        "SELECT * FROM a RIGHT JOIN b ON a.id = b.a_id",
        "SELECT * FROM a FULL JOIN b ON a.id = b.a_id",
    ] {
        let output = Command::new(CLI)
            .arg("query")
            .arg(&db)
            .arg(sql)
            .output()
            .unwrap_or_else(|e| panic!("running {CLI} query {}: {e}", db.display()));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "expected {sql:?} to fail");
        assert!(
            stderr.contains("not yet supported"),
            "expected an unsupported-construct diagnostic for {sql:?}; got: {stderr}"
        );
        assert!(!stderr.contains("panicked at"), "must not panic: {stderr}");
    }
}

/// #243: `EXPLAIN QUERY PLAN` reports `SEARCH ... USING INTEGER PRIMARY
/// KEY` for the rowid-seek join path and `SCAN` for tables that still
/// fall back to a full scan (no matching index on the equality side).
#[test]
fn explain_query_plan_reports_rowid_search_and_scan() {
    let db = join_fixture_db("eqp_rowid");
    let output = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM b JOIN a ON b.a_id = a.id",
    );
    assert_eq!(
        output, "0|0|0|SCAN b\n1|0|0|SEARCH a USING INTEGER PRIMARY KEY (rowid=?)\n",
        "outer table b is a full scan; inner table a is seeked by rowid"
    );
}

/// #243: `EXPLAIN QUERY PLAN` reports `SEARCH ... USING INDEX <name>`
/// for the unique-secondary-index seek join path.
#[test]
fn explain_query_plan_reports_unique_index_search() {
    let db = scratch_db("eqp_unique_index");
    let ddls = [
        "CREATE TABLE d(id INTEGER PRIMARY KEY, code TEXT)",
        "CREATE UNIQUE INDEX idx_d_code ON d(code)",
        "CREATE TABLE e(id INTEGER PRIMARY KEY, d_code TEXT)",
    ];
    if let Some(oracle) = pinned_oracle() {
        for stmt in ddls {
            let status = Command::new(&oracle).arg(&db).arg(stmt).status().unwrap();
            assert!(status.success(), "oracle setup failed: {stmt}");
        }
    } else {
        assert!(run_exec(&db, "CREATE TABLE seed_bootstrap(x)")
            .status
            .success());
        for stmt in ddls {
            let output = run_exec(&db, stmt);
            assert!(
                output.status.success(),
                "setup {stmt:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    let output = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM e JOIN d ON e.d_code = d.code",
    );
    assert_eq!(
        output,
        "0|0|0|SCAN e\n1|0|0|SEARCH d USING INDEX idx_d_code (code=?)\n",
    );
}

/// #243: without a usable index/rowid equality, `EXPLAIN QUERY PLAN`
/// reports `SCAN` for every joined table — the full-scan fallback must
/// stay observable, not just correct.
#[test]
fn explain_query_plan_reports_full_scan_fallback() {
    let db = join_fixture_db("eqp_full_scan");
    let output = run_query(
        &db,
        "EXPLAIN QUERY PLAN SELECT * FROM a JOIN b ON a.name = b.tag",
    );
    assert_eq!(output, "0|0|0|SCAN a\n1|0|0|SCAN b\n");
}
