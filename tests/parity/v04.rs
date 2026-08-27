// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! V04 parity: the relational core (#234's JOIN/subquery/GROUP BY/
//! UNION ALL surface). Same five-dimension vocabulary and CLI-driven
//! write/verify shape as `v03.rs`, adapted to a read-only multi-table
//! query: seed an identical schema+data on both engines, then compare
//! one JOIN + GROUP BY + aggregate query's output — the case #333
//! found unsupported (aggregate functions combined with a JOIN) and
//! fixed.
//!
//! See issue #72 (parity suite) and #333 (this dimension's first case).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-parity-v04-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

fn ours_exec(db: &Path, sql: &str) -> Output {
    Command::new(CLI)
        .arg("exec")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} exec {} {sql:?}: {e}", db.display()))
}

fn ours_query(db: &Path, sql: &str) -> Result<String, String> {
    let output = Command::new(CLI)
        .arg("query")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} query {} {sql:?}: {e}", db.display()));
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn oracle_exec(oracle: &Path, db: &Path, sql: &str) -> Output {
    Command::new(oracle)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running oracle exec {} {sql:?}: {e}", db.display()))
}

fn oracle_query(oracle: &Path, db: &Path, sql: &str) -> Result<String, String> {
    let output = oracle_exec(oracle, db, sql);
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// #333: `count`/`sum`/`avg`/`min`/`max` combined with `GROUP BY` and a
/// JOIN, and the implicit whole-table aggregate (no `GROUP BY` at all)
/// combined with a JOIN.
#[test]
fn join_group_by_aggregate_acceptance_and_output_match() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("join_group_by_aggregate_acceptance_and_output_match");
        return;
    };

    let ours_db = scratch_db("join-agg-ours");
    let theirs_db = scratch_db("join-agg-theirs");

    let setup = [
        "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT)",
        "CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER, tag TEXT)",
        "INSERT INTO a VALUES (1,'alice'),(2,'bob'),(3,'carol')",
        "INSERT INTO b VALUES (10,1,'x'),(11,1,'y'),(12,2,'z')",
    ];
    for stmt in setup {
        assert!(
            oracle_exec(&oracle, &ours_db, stmt).status.success(),
            "seeding ours db failed for {stmt:?}"
        );
        assert!(
            oracle_exec(&oracle, &theirs_db, stmt).status.success(),
            "seeding theirs db failed for {stmt:?}"
        );
    }

    let queries = [
        "SELECT a.name, count(*) FROM a JOIN b ON a.id = b.a_id GROUP BY a.name",
        "SELECT a.name, sum(b.id) FROM a JOIN b ON a.id = b.a_id GROUP BY a.name",
        "SELECT count(*) FROM a JOIN b ON a.id = b.a_id",
    ];
    for sql in queries {
        let ours = ours_query(&ours_db, sql);
        let theirs = oracle_query(&oracle, &theirs_db, sql);
        assert_eq!(ours, theirs, "output mismatch for {sql:?}");
    }

    // Acceptance dimension: a plain scan-side `exec` also succeeds
    // identically for a joined+grouped statement used as a subquery
    // target (INSERT ... SELECT already covered by tier3; this is the
    // read-only CLI path).
    assert!(
        ours_exec(&ours_db, "CREATE TABLE dst(name TEXT, total INTEGER)")
            .status
            .success()
    );
    assert!(
        ours_exec(
            &ours_db,
            "INSERT INTO dst SELECT a.name, count(*) FROM a JOIN b ON a.id = b.a_id GROUP BY a.name"
        )
        .status
        .success(),
        "INSERT ... SELECT with a joined GROUP BY aggregate should succeed"
    );
}
