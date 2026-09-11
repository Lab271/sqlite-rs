// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #687 acceptance: `CREATE TABLE` run through this crate must emit a
//! `sqlite_autoindex_*` for every constraint stock SQLite would
//! autoindex — a declared composite `PRIMARY KEY`/`UNIQUE`, or a
//! non-alias single-column table-level `PRIMARY KEY`. Before this fix,
//! no index b-tree or `sqlite_master` row was created at all, so the
//! oracle reported "database disk image is malformed (11)" on any write
//! or `integrity_check` against a table this crate created with a
//! composite key.
//!
//! This is the producing half of #685 (which fixed adopting an
//! oracle-created autoindex); the numbering rule is shared and already
//! tested there.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-create-autoindex-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

fn run_ours(db: &Path, sql: &str) -> Output {
    Command::new(CLI)
        .arg("query")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} query {} {sql:?}: {e}", db.display()))
}

fn create_via_ours(db: &Path, ddl: &str) {
    let output = Command::new(CLI)
        .arg("exec")
        .arg(db)
        .arg(ddl)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} exec {} {ddl:?}: {e}", db.display()));
    assert!(
        output.status.success(),
        "CREATE TABLE {ddl:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn oracle_query(oracle: &Path, db: &Path, sql: &str) -> String {
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

fn oracle_exec_ok(oracle: &Path, db: &Path, sql: &str) {
    let output = Command::new(oracle)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running oracle exec on {}: {e}", db.display()));
    assert!(
        output.status.success(),
        "oracle exec {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn oracle_exec_fails(oracle: &Path, db: &Path, sql: &str) {
    let output = Command::new(oracle)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running oracle exec on {}: {e}", db.display()));
    assert!(
        !output.status.success(),
        "expected oracle exec {sql:?} to fail, but it succeeded"
    );
}

/// A table created here with a composite `PRIMARY KEY` must pass the
/// oracle's `PRAGMA integrity_check` and accept an oracle write —
/// the issue's headline "database disk image is malformed (11)" symptom.
#[test]
fn composite_primary_key_passes_oracle_integrity_check_and_accepts_oracle_writes() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("create_table_autoindex");
        return;
    };
    let db = scratch_db("composite-pk");
    create_via_ours(
        &db,
        "CREATE TABLE t (a INTEGER, b TEXT, PRIMARY KEY (a, b))",
    );
    assert_integrity_check_ok(&oracle, &db);
    oracle_exec_ok(&oracle, &db, "INSERT INTO t VALUES (1, 'x')");
    assert_integrity_check_ok(&oracle, &db);
    // The autoindex must actually enforce uniqueness for the oracle too.
    oracle_exec_fails(&oracle, &db, "INSERT INTO t VALUES (1, 'x')");
}

/// A declared composite `UNIQUE` constraint gets the same treatment as a
/// composite `PRIMARY KEY`.
#[test]
fn composite_unique_passes_oracle_integrity_check_and_accepts_oracle_writes() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("create_table_autoindex");
        return;
    };
    let db = scratch_db("composite-unique");
    create_via_ours(&db, "CREATE TABLE t (a INTEGER, b TEXT, UNIQUE (a, b))");
    assert_integrity_check_ok(&oracle, &db);
    oracle_exec_ok(&oracle, &db, "INSERT INTO t VALUES (1, 'x')");
    assert_integrity_check_ok(&oracle, &db);
    oracle_exec_fails(&oracle, &db, "INSERT INTO t VALUES (1, 'x')");
}

/// `sqlite_master`'s autoindex row must match the oracle's own
/// conventions: `type = 'index'`, the `sqlite_autoindex_<table>_<n>`
/// name, `tbl_name` the owning table, and — the detail the naive old
/// `MasterEntry` couldn't represent — `sql IS NULL`, not an empty string.
#[test]
fn autoindex_master_row_shape_matches_oracle_convention() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("create_table_autoindex");
        return;
    };
    let db = scratch_db("master-row-shape");
    create_via_ours(
        &db,
        "CREATE TABLE t (a INTEGER, b TEXT, PRIMARY KEY (a, b))",
    );
    let rows = oracle_query(
        &oracle,
        &db,
        "SELECT type, name, tbl_name, sql IS NULL FROM sqlite_master WHERE type = 'index'",
    );
    assert_eq!(rows.trim(), "index|sqlite_autoindex_t_1|t|1");
}

/// A non-`WITHOUT ROWID` single-column table-level `PRIMARY KEY` on an
/// `INTEGER` column is a rowid alias (#686) and must get no autoindex at
/// all — the numbering rule's "consumes no number" clause.
#[test]
fn rowid_alias_primary_key_gains_no_autoindex() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("create_table_autoindex");
        return;
    };
    let db = scratch_db("rowid-alias-no-index");
    create_via_ours(&db, "CREATE TABLE t (a INTEGER, b TEXT, PRIMARY KEY (a))");
    assert_integrity_check_ok(&oracle, &db);
    let count = oracle_query(
        &oracle,
        &db,
        "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = 't'",
    );
    assert_eq!(count.trim(), "0");
}

/// `WITHOUT ROWID`'s own primary key is the table itself and must gain
/// no separate autoindex either.
///
/// Deliberately does not assert `integrity_check` here: this crate
/// still stores a `WITHOUT ROWID` table's own b-tree as an ordinary
/// rowid table b-tree rather than the index b-tree the file format
/// requires (a pre-existing gap, independent of autoindexing — full
/// `WITHOUT ROWID` write support is unimplemented, not this ticket's
/// scope). Only the autoindex-count claim in #687's scope is checked.
#[test]
fn without_rowid_primary_key_gains_no_autoindex() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("create_table_autoindex");
        return;
    };
    let db = scratch_db("without-rowid-no-index");
    create_via_ours(
        &db,
        "CREATE TABLE t (a INTEGER, b TEXT, PRIMARY KEY (a)) WITHOUT ROWID",
    );
    let count = oracle_query(
        &oracle,
        &db,
        "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = 't'",
    );
    assert_eq!(count.trim(), "0");
}

/// Full round trip: create here, write with the oracle, read back here.
#[test]
fn round_trips_create_here_write_oracle_read_here() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("create_table_autoindex");
        return;
    };
    let db = scratch_db("round-trip");
    create_via_ours(
        &db,
        "CREATE TABLE t (a INTEGER, b TEXT, PRIMARY KEY (a, b))",
    );
    oracle_exec_ok(&oracle, &db, "INSERT INTO t VALUES (1, 'x')");
    let output = run_ours(&db, "SELECT a, b FROM t");
    assert!(
        output.status.success(),
        "read-back failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "1|x");
}
