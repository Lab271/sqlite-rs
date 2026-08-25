//! V05 parity: core transactions (#356/#360's BEGIN/COMMIT/ROLLBACK
//! surface). Same CLI-driven write/verify shape as `v04.rs`: seed an
//! identical schema+data on both engines via a shared oracle-authored
//! setup script, then run a transactional statement sequence through
//! our own `exec` (one process, one `;`-separated script, shared pager
//! and autocommit state across statements — see `exec.rs`'s module
//! doc) and compare the resulting table contents against the oracle
//! running the same script.
//!
//! See issue #72 (parity suite) and V5 Slim (`.openspec/plan.md`):
//! BEGIN/COMMIT/ROLLBACK plus DEFERRED/IMMEDIATE/EXCLUSIVE, journal
//! mode DELETE.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-parity-v05-{label}-{}-{n}",
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

const SETUP: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)";

/// A `BEGIN`/write/`COMMIT` script durably applies its writes, matching
/// the oracle running the identical script.
#[test]
fn commit_persists_writes_acceptance_and_output_match() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("commit_persists_writes_acceptance_and_output_match");
        return;
    };

    let ours_db = scratch_db("commit-ours");
    let theirs_db = scratch_db("commit-theirs");
    assert!(oracle_exec(&oracle, &ours_db, SETUP).status.success());
    assert!(oracle_exec(&oracle, &theirs_db, SETUP).status.success());

    let script = "BEGIN; INSERT INTO t VALUES (1,'a'); INSERT INTO t VALUES (2,'b'); COMMIT;";
    assert!(
        ours_exec(&ours_db, script).status.success(),
        "our commit script should succeed"
    );
    assert!(
        oracle_exec(&oracle, &theirs_db, script).status.success(),
        "oracle commit script should succeed"
    );

    let sql = "SELECT id, val FROM t ORDER BY id";
    assert_eq!(
        ours_query(&ours_db, sql),
        oracle_query(&oracle, &theirs_db, sql)
    );
}

/// A `BEGIN`/write/`ROLLBACK` script leaves the table exactly as it was
/// before the transaction — matching the oracle's rollback behavior.
#[test]
fn rollback_discards_writes_acceptance_and_output_match() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("rollback_discards_writes_acceptance_and_output_match");
        return;
    };

    let ours_db = scratch_db("rollback-ours");
    let theirs_db = scratch_db("rollback-theirs");
    let seed = "CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT); INSERT INTO t VALUES (1,'seed');";
    assert!(oracle_exec(&oracle, &ours_db, seed).status.success());
    assert!(oracle_exec(&oracle, &theirs_db, seed).status.success());

    let script = "BEGIN; INSERT INTO t VALUES (2,'doomed'); DELETE FROM t WHERE id=1; ROLLBACK;";
    assert!(
        ours_exec(&ours_db, script).status.success(),
        "our rollback script should succeed"
    );
    assert!(
        oracle_exec(&oracle, &theirs_db, script).status.success(),
        "oracle rollback script should succeed"
    );

    let sql = "SELECT id, val FROM t ORDER BY id";
    assert_eq!(
        ours_query(&ours_db, sql),
        oracle_query(&oracle, &theirs_db, sql)
    );
}

/// DEFERRED/IMMEDIATE/EXCLUSIVE all parse and behave like a plain
/// `BEGIN` for a single-connection commit — no distinct outcome to
/// diverge on here, but acceptance (does it run at all) must match.
#[test]
fn begin_mode_keywords_acceptance_and_output_match() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("begin_mode_keywords_acceptance_and_output_match");
        return;
    };

    for mode in ["DEFERRED", "IMMEDIATE", "EXCLUSIVE"] {
        let ours_db = scratch_db(&format!("begin-mode-ours-{mode}"));
        let theirs_db = scratch_db(&format!("begin-mode-theirs-{mode}"));
        assert!(oracle_exec(&oracle, &ours_db, SETUP).status.success());
        assert!(oracle_exec(&oracle, &theirs_db, SETUP).status.success());

        let script = format!("BEGIN {mode}; INSERT INTO t VALUES (1,'x'); COMMIT;");
        let ours_ok = ours_exec(&ours_db, &script).status.success();
        let theirs_ok = oracle_exec(&oracle, &theirs_db, &script).status.success();
        assert_eq!(ours_ok, theirs_ok, "acceptance mismatch for BEGIN {mode}");
        assert!(ours_ok, "BEGIN {mode} should succeed");

        let sql = "SELECT id, val FROM t ORDER BY id";
        assert_eq!(
            ours_query(&ours_db, sql),
            oracle_query(&oracle, &theirs_db, sql),
            "output mismatch for BEGIN {mode}"
        );
    }
}
