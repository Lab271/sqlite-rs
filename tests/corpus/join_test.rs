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
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-join-{label}-{}-{n}",
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
        output,
        "1|alice|10|x\n1|alice|11|y\n2|bob||\n3|carol||\n",
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
    let output = run_query(&db, "SELECT a.id, b.id FROM a JOIN b ON a.id = b.a_id LIMIT 1");
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
