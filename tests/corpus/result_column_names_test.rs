// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #709: `.headers on` column labels for a join or a compound
//! `SELECT` used to fall back to positional `column1`/`column2`
//! placeholders — `derive_headers` (`src/bin/sqlite-rs/repl.rs`) had
//! no join-aware naming, and never even tried a compound (despite
//! `output_column_names` already implementing the "leftmost arm" rule
//! internally, for a compound's own trailing `ORDER BY` resolution).
//! `output_column_names_joined` (`src/codegen/select/order_by.rs`)
//! closes the join gap; routing a compound through the existing
//! single-arm `output_column_names` closes the other. Oracle-diffed
//! against the pinned 3.53.4 `sqlite3 -header`.

use crate::oracle::pinned_oracle;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str, oracle: &Path, setup_sql: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_result_column_names_test_{}_{n}_{label}.db",
        std::process::id()
    ));
    std::fs::remove_file(&path).ok();
    let status = Command::new(oracle)
        .arg(&path)
        .arg(setup_sql)
        .status()
        .expect("creating scratch fixture db");
    assert!(status.success());
    path
}

/// The oracle's own header row for `sql`, via one-shot `-header`.
fn oracle_header(oracle: &Path, db: &Path, sql: &str) -> String {
    let output = Command::new(oracle)
        .arg("-readonly")
        .arg("-header")
        .arg("-separator")
        .arg("|")
        .arg(db)
        .arg(sql)
        .output()
        .expect("invoking sqlite3 oracle");
    assert!(
        output.status.success(),
        "oracle failed on {sql:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Our own header row for `sql`, via the REPL's `.headers on`/list
/// mode. The REPL echoes a bare `sqlite> ` prompt per stdin line read
/// before any of that line's own output, with no intervening newline —
/// stripped out here so the remaining first non-blank line is the
/// header row itself.
fn our_header(db: &Path, sql: &str) -> String {
    let script = format!(".headers on\n.mode list\n{sql};\n.quit\n");
    let mut child = Command::new(CLI)
        .arg("repl")
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning sqlite-rs repl");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("waiting on repl");
    assert!(
        output.status.success(),
        "repl failed on {sql:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).replace("sqlite> ", "");
    stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .to_string()
}

fn assert_header_matches_oracle(oracle: &Path, db: &Path, sql: &str) {
    assert_eq!(
        our_header(db, sql),
        oracle_header(oracle, db, sql),
        "{sql:?}"
    );
}

/// The core defect: a two-table join used to report
/// `column1|column2`.
#[test]
fn two_table_join_reports_real_column_names() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "join2",
        &oracle,
        "CREATE TABLE a(x INTEGER, y TEXT); CREATE TABLE b(x INTEGER, z TEXT); \
         INSERT INTO a VALUES (1, 'p'); INSERT INTO b VALUES (1, 'q');",
    );
    assert_header_matches_oracle(&oracle, &db, "SELECT a.x, b.z FROM a JOIN b ON a.x = b.x");
}

/// A three-table join.
#[test]
fn three_table_join_reports_real_column_names() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "join3",
        &oracle,
        "CREATE TABLE a(x INTEGER); CREATE TABLE b(x INTEGER, y TEXT); \
         CREATE TABLE c(x INTEGER, z TEXT); INSERT INTO a VALUES (1); \
         INSERT INTO b VALUES (1, 'p'); INSERT INTO c VALUES (1, 'q');",
    );
    assert_header_matches_oracle(
        &oracle,
        &db,
        "SELECT a.x, b.y, c.z FROM a JOIN b ON a.x = b.x JOIN c ON a.x = c.x",
    );
}

/// `UNION` takes its names from the leftmost arm.
#[test]
fn union_reports_leftmost_arm_names() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "union",
        &oracle,
        "CREATE TABLE a(x INTEGER); CREATE TABLE b(y INTEGER); \
         INSERT INTO a VALUES (1); INSERT INTO b VALUES (2);",
    );
    assert_header_matches_oracle(&oracle, &db, "SELECT x FROM a UNION SELECT y FROM b");
}

/// `UNION ALL` likewise.
#[test]
fn union_all_reports_leftmost_arm_names() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "union_all",
        &oracle,
        "CREATE TABLE a(x INTEGER); CREATE TABLE b(y INTEGER); \
         INSERT INTO a VALUES (1); INSERT INTO b VALUES (2);",
    );
    assert_header_matches_oracle(&oracle, &db, "SELECT x FROM a UNION ALL SELECT y FROM b");
}

/// A subquery in `FROM`.
#[test]
fn subquery_in_from_reports_real_column_names() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "subquery",
        &oracle,
        "CREATE TABLE a(x INTEGER, y TEXT); INSERT INTO a VALUES (1, 'p');",
    );
    assert_header_matches_oracle(&oracle, &db, "SELECT s.x FROM (SELECT x, y FROM a) s");
}

/// An alias wins over a derived name.
#[test]
fn alias_wins_over_derived_name() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "alias",
        &oracle,
        "CREATE TABLE a(x INTEGER); INSERT INTO a VALUES (1);",
    );
    assert_header_matches_oracle(&oracle, &db, "SELECT a.x AS renamed FROM a");
}

/// `SELECT a.x` reports `x`, not `a.x`.
#[test]
fn qualified_reference_reports_bare_column_name() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "qualified",
        &oracle,
        "CREATE TABLE a(x INTEGER); INSERT INTO a VALUES (1);",
    );
    assert_header_matches_oracle(&oracle, &db, "SELECT a.x FROM a");
}

/// `*` expansion across a join draws each table's own columns.
#[test]
fn star_expansion_across_join_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "star_join",
        &oracle,
        "CREATE TABLE a(x INTEGER, y TEXT); CREATE TABLE b(x INTEGER, z TEXT); \
         INSERT INTO a VALUES (1, 'p'); INSERT INTO b VALUES (1, 'q');",
    );
    assert_header_matches_oracle(&oracle, &db, "SELECT * FROM a JOIN b ON a.x = b.x");
}

/// Duplicate names are legal: `SELECT a.x, b.x` yields two columns
/// both named `x`.
#[test]
fn duplicate_names_across_a_join_are_legal() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping: no pinned 3.53.4 sqlite3 oracle on this machine");
        return;
    };
    let db = scratch_db(
        "dup",
        &oracle,
        "CREATE TABLE a(x INTEGER); CREATE TABLE b(x INTEGER); \
         INSERT INTO a VALUES (1); INSERT INTO b VALUES (1);",
    );
    let header = our_header(&db, "SELECT a.x, b.x FROM a JOIN b ON a.x = b.x");
    assert_eq!(header, "x|x");
    assert_header_matches_oracle(&oracle, &db, "SELECT a.x, b.x FROM a JOIN b ON a.x = b.x");
}
