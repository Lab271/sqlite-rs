// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #707: `SELECT ... FROM sqlite_master` didn't compile at all — the
//! catalog `read_schema` decodes was never reachable as a *table* from
//! the `SELECT` path. `sqlite_master` is a real b-tree at page 1 with a
//! known five-column shape, so `resolve_from_table_schema` now hands
//! back a hardcoded schema for it and the rest of codegen treats it as
//! an ordinary table scan — no synthesized rows. Oracle-diffed through
//! the `sqlite-rs` CLI's `query` subcommand against the pinned 3.53.4
//! `sqlite3`.

use crate::oracle::{pinned_oracle, run_oracle, skip_no_oracle};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str, oracle: &Path, setup_sql: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_sqlite_master_test_{}_{n}_{label}.db",
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

fn our_query(db: &Path, sql: &str) -> String {
    let output = Command::new(CLI)
        .arg("query")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running sqlite-rs query: {e}"));
    assert!(
        output.status.success(),
        "sqlite-rs query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The core defect: a plain `SELECT ... FROM sqlite_master`, over a
/// database with tables, an explicit index and an implicit
/// (`UNIQUE`-constraint) autoindex — every row shape `sqlite_master`
/// can hold.
#[test]
fn select_from_sqlite_master_matches_oracle_including_indexes_and_autoindexes() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle(
            "select_from_sqlite_master_matches_oracle_including_indexes_and_autoindexes",
        );
        return;
    };
    let db = scratch_db(
        "basic",
        &oracle,
        "CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_b ON t(b); \
         CREATE TABLE u(x TEXT UNIQUE);",
    );
    let sql = "SELECT type, name, tbl_name, rootpage, sql FROM sqlite_master ORDER BY name";
    let ours = our_query(&db, sql);
    let expected = run_oracle(&oracle, &db, &[], sql);
    assert_eq!(ours, expected);
}

/// The consumer's actual index-discovery case: filtering by `type`.
#[test]
fn select_name_where_type_is_index_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("select_name_where_type_is_index_matches_oracle");
        return;
    };
    let db = scratch_db(
        "index_filter",
        &oracle,
        "CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_b ON t(b); \
         CREATE TABLE u(x TEXT UNIQUE);",
    );
    let sql = "SELECT name FROM sqlite_master WHERE type = 'index' ORDER BY name";
    let ours = our_query(&db, sql);
    let expected = run_oracle(&oracle, &db, &[], sql);
    assert_eq!(ours, expected);
}

/// `sqlite_schema` is the modern alias for the same table.
#[test]
fn sqlite_schema_alias_resolves_to_the_same_table() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("sqlite_schema_alias_resolves_to_the_same_table");
        return;
    };
    let db = scratch_db("alias", &oracle, "CREATE TABLE t(a INTEGER);");
    let sql = "SELECT type, name FROM sqlite_schema ORDER BY name";
    let ours = our_query(&db, sql);
    let expected = run_oracle(&oracle, &db, &[], sql);
    assert_eq!(ours, expected);
}

/// An empty database (no user objects at all) returns zero rows, not
/// an error.
#[test]
fn empty_database_returns_zero_rows() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("empty_database_returns_zero_rows");
        return;
    };
    // Force a real header to exist even with no surviving objects.
    let db = scratch_db(
        "empty",
        &oracle,
        "CREATE TABLE tmp(x); DROP TABLE tmp; VACUUM;",
    );
    let sql = "SELECT * FROM sqlite_master";
    let ours = our_query(&db, sql);
    let expected = run_oracle(&oracle, &db, &[], sql);
    assert_eq!(ours, expected);
    assert_eq!(ours, "");
}
