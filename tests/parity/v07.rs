//! V07 parity: correctness of the V7.2 planner/performance work
//! (`.openspec/plan.md` epic #421, phase V7.2) — join ordering
//! heuristics (#462/#470), skip-scan for non-leading index columns
//! (#485), and `ORDER BY`/`LIMIT` on a compound (`UNION`) `SELECT`
//! (#484). These are all cost-model/codegen changes that must be
//! output-transparent: activating a different plan should never
//! change what a query returns. Same CLI-driven write/verify shape as
//! `v04.rs`/`v05.rs`/`v06.rs`.
//!
//! See issue #72 (parity suite) and #421 (V7 epic).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-parity-v07-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
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

/// #462/#470: join ordering heuristics driven by the ANALYZE-derived
/// cost model (#461) must pick a plan that still returns the correct
/// rows — a differently-ordered join over a small/large table pair,
/// where the heuristic has an actual choice to make.
#[test]
fn join_ordering_heuristic_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("join_ordering_heuristic_matches_oracle");
        return;
    };

    let ours_db = scratch_db("join-order-ours");
    let theirs_db = scratch_db("join-order-theirs");

    let mut setup = vec![
        "CREATE TABLE small(id INTEGER PRIMARY KEY, tag TEXT)".to_owned(),
        "INSERT INTO small VALUES (1,'x'),(2,'y')".to_owned(),
        "CREATE TABLE large(id INTEGER PRIMARY KEY, small_id INTEGER, v INTEGER)".to_owned(),
    ];
    for i in 0..200 {
        setup.push(format!(
            "INSERT INTO large VALUES ({}, {}, {})",
            i,
            (i % 2) + 1,
            i * 3
        ));
    }
    setup.push("ANALYZE".to_owned());

    for stmt in &setup {
        assert!(oracle_exec(&oracle, &ours_db, stmt).status.success());
        assert!(oracle_exec(&oracle, &theirs_db, stmt).status.success());
    }

    let sql = "SELECT small.tag, count(*) FROM small JOIN large ON small.id = large.small_id \
               WHERE large.v > 100 GROUP BY small.tag";
    assert_eq!(
        ours_query(&ours_db, sql),
        oracle_query(&oracle, &theirs_db, sql)
    );
}

/// #485: skip-scan for a query that filters on a non-leading index
/// column must still return exactly the rows a full scan would.
#[test]
fn skip_scan_non_leading_index_column_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("skip_scan_non_leading_index_column_matches_oracle");
        return;
    };

    let ours_db = scratch_db("skip-scan-ours");
    let theirs_db = scratch_db("skip-scan-theirs");

    let mut setup = vec![
        "CREATE TABLE t(a INTEGER, b INTEGER, v TEXT)".to_owned(),
        "CREATE INDEX idx_ab ON t(a, b)".to_owned(),
    ];
    for i in 0..100 {
        setup.push(format!(
            "INSERT INTO t VALUES ({}, {}, 'row{}')",
            i % 3,
            i,
            i
        ));
    }
    setup.push("ANALYZE".to_owned());

    for stmt in &setup {
        assert!(oracle_exec(&oracle, &ours_db, stmt).status.success());
        assert!(oracle_exec(&oracle, &theirs_db, stmt).status.success());
    }

    // Filters only on `b`, the non-leading column of idx_ab — the
    // shape #485's skip-scan targets.
    let sql = "SELECT a, b, v FROM t WHERE b = 50 ORDER BY a, b";
    assert_eq!(
        ours_query(&ours_db, sql),
        oracle_query(&oracle, &theirs_db, sql)
    );
}

/// #484: `ORDER BY`/`LIMIT` applied to a compound (`UNION`) `SELECT`
/// must sort/limit the merged result set, not just one arm.
#[test]
fn compound_select_order_by_limit_matches_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("compound_select_order_by_limit_matches_oracle");
        return;
    };

    let ours_db = scratch_db("compound-orderby-ours");
    let theirs_db = scratch_db("compound-orderby-theirs");

    let setup = [
        "CREATE TABLE t1(v INTEGER)".to_owned(),
        "CREATE TABLE t2(v INTEGER)".to_owned(),
        "INSERT INTO t1 VALUES (5),(1),(9)".to_owned(),
        "INSERT INTO t2 VALUES (2),(8),(4)".to_owned(),
    ];
    for stmt in &setup {
        assert!(oracle_exec(&oracle, &ours_db, stmt).status.success());
        assert!(oracle_exec(&oracle, &theirs_db, stmt).status.success());
    }

    let sql = "SELECT v FROM t1 UNION ALL SELECT v FROM t2 ORDER BY v DESC LIMIT 4";
    assert_eq!(
        ours_query(&ours_db, sql),
        oracle_query(&oracle, &theirs_db, sql)
    );
}
