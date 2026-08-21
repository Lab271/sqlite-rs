#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Tier 3 — everything else, droppable in the defined order (`plan.md`
//! Core Definition & Drop Order). One ignored stub per drop-order entry,
//! so the drop list itself stays executable: flipping a stub live is the
//! acceptance bar for the ticket that lands that entry.

#[path = "../corpus/oracle.rs"]
#[allow(dead_code)]
mod oracle;

use oracle::pinned_oracle;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-tier3-{label}-{}-{n}",
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

/// #250 — the exit-gate proof that V4's multi-table read slice (JOIN
/// parsing/codegen for every join form, plus ORDER BY/DISTINCT/
/// `INSERT ... SELECT` generalized to a joined `Scope`) holds together
/// end-to-end: a JOIN combined with both ORDER BY and (separately)
/// DISTINCT, plus an `INSERT ... SELECT` copying joined rows into a
/// fresh table.
#[test]
fn t3_multi_table_joins_and_aggregates() {
    let db = scratch_db("joins");
    let ddls = [
        "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT)",
        "CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER, tag TEXT)",
    ];
    let rows = [
        "INSERT INTO a VALUES (1,'alice'),(2,'bob'),(3,'carol')",
        "INSERT INTO b VALUES (10,1,'x'),(11,1,'y'),(12,2,'z')",
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
            assert!(output.status.success(), "setup {stmt:?} failed");
        }
    }

    // JOIN + ORDER BY, keyed on a column from the joined table.
    let ordered = run_query(
        &db,
        "SELECT a.name, b.tag FROM a JOIN b ON a.id = b.a_id ORDER BY b.tag DESC",
    );
    assert_eq!(ordered, "bob|z\nalice|y\nalice|x\n");

    // JOIN + DISTINCT collapses `alice`'s two matching `b` rows.
    let distinct = run_query(&db, "SELECT DISTINCT a.name FROM a JOIN b ON a.id = b.a_id");
    assert_eq!(
        distinct.lines().count(),
        2,
        "alice+bob, deduplicated: {distinct}"
    );

    // GROUP BY + aggregate over the multi-table dataset above, with the
    // aggregate combined with the JOIN itself (#333) — `a`'s two
    // matched ids fan out 2/1 across `b`'s rows.
    let aggregated = run_query(
        &db,
        "SELECT a.name, count(*) FROM a JOIN b ON a.id = b.a_id GROUP BY a.name",
    );
    let mut aggregated_lines: Vec<&str> = aggregated.lines().collect();
    aggregated_lines.sort_unstable();
    assert_eq!(aggregated_lines, ["alice|2", "bob|1"]);

    // INSERT ... SELECT with a JOIN on the SELECT side.
    assert!(run_exec(&db, "CREATE TABLE dst(name TEXT, tag TEXT)")
        .status
        .success());
    assert!(run_exec(
        &db,
        "INSERT INTO dst SELECT a.name, b.tag FROM a JOIN b ON a.id = b.a_id"
    )
    .status
    .success());
    assert_eq!(run_query(&db, "SELECT * FROM dst").lines().count(), 3);
}

#[test]
#[ignore = "drop-order 2 (V6) — WAL writing (WAL reading is Tier 0)"]
fn t3_wal_writing_and_live_interop() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 3 (V8) — foreign keys + triggers"]
fn t3_foreign_keys_and_triggers() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 4 (V9) — UPSERT / RETURNING / window functions"]
fn t3_modern_sql_upsert_returning_windows() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 5 — PRAGMAs beyond introspection"]
fn t3_pragmas_beyond_introspection() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 6 — ALTER TABLE, VACUUM"]
fn t3_alter_table_and_vacuum() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 7 (V10) — writing to WITHOUT ROWID / STRICT tables (reading them is Tier 0)"]
fn t3_writes_to_without_rowid_and_strict_tables() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 8 (V11) — vtab/JSON extension semantics: ATTACH, sessions, hooks"]
fn t3_attach_sessions_and_hooks() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 8 (V12) — FTS5/R-Tree query-level extension semantics"]
fn t3_fts5_and_rtree_query_semantics() {
    unimplemented!()
}
