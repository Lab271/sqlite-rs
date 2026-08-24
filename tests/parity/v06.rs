//! V06 parity: WAL journal mode plus the basic-relational surface
//! deferred alongside it — non-recursive `WITH`/CTE, `UNION` (dedup),
//! and `CREATE VIEW` (`.openspec/plan.md`'s V6 Slim scope). Same
//! CLI-driven write/verify shape as `v04.rs`/`v05.rs`.
//!
//! See issue #72 (parity suite). WAL/SHM interop with a *live* stock
//! sqlite3 process is its own dedicated suite
//! (`tests/corpus/wal_concurrent_interop_test.rs`); this file only
//! checks that switching journal_mode=WAL and reading/writing through
//! it round-trips identically to the oracle, one process at a time.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-parity-v06-{label}-{}-{n}",
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

const SETUP: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER); \
     INSERT INTO t VALUES (1,10),(2,20),(3,30);";

/// Switching a database to `journal_mode=WAL` and writing through it
/// produces the same on-query-readback contents as the oracle doing
/// the same switch-then-write, one process at a time (live-interop
/// with a concurrently running oracle process is covered separately by
/// `wal_concurrent_interop_test.rs`).
#[test]
fn wal_mode_write_then_read_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("wal_mode_write_then_read_matches_oracle");
        return;
    };

    let ours_db = scratch_db("wal-ours");
    let theirs_db = scratch_db("wal-theirs");
    assert!(oracle_exec(&oracle, &ours_db, SETUP).status.success());
    assert!(oracle_exec(&oracle, &theirs_db, SETUP).status.success());

    let script =
        "PRAGMA journal_mode=WAL; INSERT INTO t VALUES (4,40); UPDATE t SET x=99 WHERE id=1;";
    assert!(
        ours_exec(&ours_db, script).status.success(),
        "our WAL write script should succeed"
    );
    assert!(
        oracle_exec(&oracle, &theirs_db, script).status.success(),
        "oracle WAL write script should succeed"
    );

    let sql = "SELECT id, x FROM t ORDER BY id";
    assert_eq!(
        ours_query(&ours_db, sql),
        oracle_query(&oracle, &theirs_db, sql)
    );
}

/// A non-recursive `WITH`/CTE query matches the oracle's output.
#[test]
fn non_recursive_cte_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("non_recursive_cte_matches_oracle");
        return;
    };

    let ours_db = scratch_db("cte-ours");
    let theirs_db = scratch_db("cte-theirs");
    assert!(oracle_exec(&oracle, &ours_db, SETUP).status.success());
    assert!(oracle_exec(&oracle, &theirs_db, SETUP).status.success());

    let sql = "WITH big AS (SELECT id, x FROM t WHERE x >= 20) SELECT id, x FROM big ORDER BY id";
    assert_eq!(
        ours_query(&ours_db, sql),
        oracle_query(&oracle, &theirs_db, sql)
    );
}

/// Plain `UNION` (dedup, as opposed to `UNION ALL`) matches the
/// oracle's deduplicated output.
#[test]
fn union_dedup_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("union_dedup_matches_oracle");
        return;
    };

    let ours_db = scratch_db("union-ours");
    let theirs_db = scratch_db("union-theirs");
    assert!(oracle_exec(&oracle, &ours_db, SETUP).status.success());
    assert!(oracle_exec(&oracle, &theirs_db, SETUP).status.success());

    let sql = "SELECT x FROM t WHERE x >= 20 UNION SELECT x FROM t WHERE x <= 20 ORDER BY x";
    assert_eq!(
        ours_query(&ours_db, sql),
        oracle_query(&oracle, &theirs_db, sql)
    );
}

/// `CREATE VIEW` plus a `SELECT` against it matches the oracle.
#[test]
fn view_select_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("view_select_matches_oracle");
        return;
    };

    let ours_db = scratch_db("view-ours");
    let theirs_db = scratch_db("view-theirs");
    assert!(oracle_exec(&oracle, &ours_db, SETUP).status.success());
    assert!(oracle_exec(&oracle, &theirs_db, SETUP).status.success());

    let view_stmt = "CREATE VIEW big AS SELECT id, x FROM t WHERE x >= 20";
    assert!(ours_exec(&ours_db, view_stmt).status.success());
    assert!(oracle_exec(&oracle, &theirs_db, view_stmt).status.success());

    let sql = "SELECT id, x FROM big ORDER BY id";
    assert_eq!(
        ours_query(&ours_db, sql),
        oracle_query(&oracle, &theirs_db, sql)
    );
}
