// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #686 acceptance: a table-level `PRIMARY KEY(col)` on a single
//! `INTEGER` column is a rowid alias regardless of how many other
//! columns the table has — only a composite key (naming more than one
//! column) rules it out. Every row of the issue's rule table
//! round-trips against the oracle: the value read back is the actual
//! value, not `NULL`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-rowid-alias-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
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

fn oracle_exec(oracle: &Path, db: &Path, sql: &str) -> Output {
    Command::new(oracle)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running oracle exec on {}: {e}", db.display()))
}

/// Runs `ddl` + `insert` against the oracle, then asserts our `SELECT`
/// matches the oracle's `SELECT`.
fn assert_round_trips(label: &str, ddl: &str, insert: &str, select: &str) {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("rowid_alias");
        return;
    };
    let db = scratch_db(label);
    for stmt in [ddl, insert] {
        let output = oracle_exec(&oracle, &db, stmt);
        assert!(
            output.status.success(),
            "oracle setup {stmt:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let ours = run_query(&db, select);
    let theirs = oracle_select(&oracle, &db, select);
    assert_eq!(ours, theirs, "mismatch for {select:?} on {ddl:?}");
}

/// Table-level `PRIMARY KEY (a)` over a single INTEGER column is the
/// rowid, whether or not the table has other columns.
#[test]
fn table_level_pk_single_column_is_rowid_alias() {
    assert_round_trips(
        "single-column",
        "CREATE TABLE m0 (a INTEGER, PRIMARY KEY (a))",
        "INSERT INTO m0 VALUES (1)",
        "SELECT a FROM m0",
    );
}

/// The issue's headline repro: a second column must not defeat the
/// rowid-alias optimization for a single-column table-level PK.
#[test]
fn table_level_pk_with_other_columns_is_still_rowid_alias() {
    assert_round_trips(
        "with-other-columns",
        "CREATE TABLE m1 (a INTEGER, b TEXT, PRIMARY KEY (a))",
        "INSERT INTO m1 VALUES (1, 'x')",
        "SELECT a, b FROM m1",
    );
}

/// A non-INTEGER typed table-level PK is never a rowid alias, so its
/// value must round-trip as an ordinary stored column too.
#[test]
fn table_level_pk_non_integer_is_not_rowid_alias() {
    assert_round_trips(
        "non-integer",
        "CREATE TABLE m2 (a TEXT, b TEXT, PRIMARY KEY (a))",
        "INSERT INTO m2 VALUES ('k', 'x')",
        "SELECT a, b FROM m2",
    );
}

/// A composite table-level key is never a rowid alias.
#[test]
fn composite_table_level_pk_is_not_rowid_alias() {
    assert_round_trips(
        "composite",
        "CREATE TABLE m3 (a INTEGER, b TEXT, PRIMARY KEY (a, b))",
        "INSERT INTO m3 VALUES (1, 'x')",
        "SELECT a, b FROM m3",
    );
}

/// Control: the column-level form was already correct.
#[test]
fn column_level_pk_is_rowid_alias() {
    assert_round_trips(
        "column-level",
        "CREATE TABLE m4 (a INTEGER PRIMARY KEY, b TEXT)",
        "INSERT INTO m4 VALUES (1, 'x')",
        "SELECT a, b FROM m4",
    );
}

// `WITHOUT ROWID` unaffected by this ticket's fix is covered at the
// `rowid_alias_from_sql` unit level (src/dump.rs's
// `rowid_alias_none_for_without_rowid`) rather than here: `WITHOUT
// ROWID` tables store rows in an index b-tree, which this crate's
// reader does not yet support independent of this ticket.
