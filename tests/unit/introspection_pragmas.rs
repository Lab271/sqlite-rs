#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! CLI-surface tests for the 9 read-only introspection pragmas (#489):
//! `table_info`, `table_list`, `index_list`, `index_info`,
//! `database_list`, `schema_version`, `user_version`, `page_size`,
//! `page_count`. Mirrors `tests/tiers/tier2.rs`'s CLI-subprocess
//! pattern (`run_exec`/`run_query` against the real `sqlite-rs`
//! binary) — the same pattern #388's `journal_mode` write-pragma
//! testing left for the parser-level unit tests in
//! `tests/unit/pragma_parser.rs`, but these pragmas have no VDBE/AST
//! path to unit-test in isolation, so the CLI surface *is* the
//! surface under test.

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
        "sqlite-rs-introspection-pragmas-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
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

/// A scratch db seeded the same way `tests/tiers/tier2.rs::seed_db`
/// is: via the pinned oracle when available (it creates the file
/// itself, unlike our own `exec`, which requires the file to already
/// exist), falling back to our own CLI's `CREATE TABLE
/// seed_bootstrap(x)` bootstrap trick otherwise.
fn seed_db(label: &str, ddls: &[&str]) -> PathBuf {
    let db = scratch_db(label);
    if let Some(oracle) = pinned_oracle() {
        for ddl in std::iter::once(&"CREATE TABLE seed_bootstrap(x)").chain(ddls) {
            let status = Command::new(&oracle).arg(&db).arg(ddl).status().unwrap();
            assert!(status.success(), "seeding {ddl:?} via oracle failed");
        }
    } else {
        assert!(run_exec(&db, "CREATE TABLE seed_bootstrap(x)")
            .status
            .success());
        for ddl in ddls {
            assert!(
                run_exec(&db, ddl).status.success(),
                "seeding {ddl:?} failed"
            );
        }
    }
    db
}

#[test]
fn table_info_reports_notnull_default_and_pk() {
    let db = seed_db(
        "table-info",
        &["CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT NOT NULL DEFAULT 'x', c REAL)"],
    );
    let rows = run_query(&db, "PRAGMA table_info(t)");
    assert_eq!(
        rows, "0|a|INTEGER|0||1\n1|b|TEXT|1|'x'|0\n2|c|REAL|0||0\n",
        "table_info(t) mismatch"
    );
}

/// `WITHOUT ROWID` composite-key columns are implicitly `NOT NULL`, and
/// `pk` carries the column's 1-based position within the key (not a
/// bare boolean) — verified against a real `sqlite3` (see
/// `pragma_query.rs`'s module-level research notes).
#[test]
fn table_info_without_rowid_composite_pk() {
    let db = seed_db(
        "table-info-wr",
        &["CREATE TABLE w (x INTEGER, y TEXT, PRIMARY KEY(x, y)) WITHOUT ROWID"],
    );
    let rows = run_query(&db, "PRAGMA table_info(w)");
    assert_eq!(rows, "0|x|INTEGER|1||1\n1|y|TEXT|1||2\n");
}

#[test]
fn table_info_unknown_table_errors() {
    let db = seed_db("table-info-missing", &[]);
    let output = Command::new(CLI)
        .arg("query")
        .arg(&db)
        .arg("PRAGMA table_info(nope)")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no such table"));
}

#[test]
fn table_list_includes_tables_and_views() {
    let db = seed_db(
        "table-list",
        &[
            "CREATE TABLE t (a INTEGER)",
            "CREATE VIEW v AS SELECT a FROM t",
        ],
    );
    let rows = run_query(&db, "PRAGMA table_list");
    let lines: Vec<&str> = rows.lines().collect();
    assert!(lines.contains(&"main|t|table|1|0|0"));
    assert!(lines.contains(&"main|v|view|0|0|0"));
}

#[test]
fn index_list_and_index_info_report_explicit_indexes() {
    let db = seed_db(
        "index-list",
        &[
            "CREATE TABLE t (a INTEGER, b TEXT)",
            "CREATE INDEX idx_t_b ON t(b)",
            "CREATE UNIQUE INDEX idx_t_a ON t(a)",
        ],
    );
    let list_rows = run_query(&db, "PRAGMA index_list(t)");
    let lines: Vec<&str> = list_rows.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines.contains(&"0|idx_t_b|0|c|0"));
    assert!(lines.contains(&"1|idx_t_a|1|c|0"));

    let info_rows = run_query(&db, "PRAGMA index_info(idx_t_b)");
    assert_eq!(info_rows, "0|1|b\n");
}

#[test]
fn database_list_reports_absolute_path() {
    let db = seed_db("database-list", &[]);
    let rows = run_query(&db, "PRAGMA database_list");
    let canonical = std::fs::canonicalize(&db).unwrap();
    assert_eq!(rows, format!("0|main|{}\n", canonical.display()));
}

#[test]
fn scalar_pragmas_return_header_fields() {
    let db = seed_db("scalars", &[]);
    assert_eq!(run_query(&db, "PRAGMA page_size").trim(), "4096");
    assert_eq!(run_query(&db, "PRAGMA user_version").trim(), "0");
    let schema_version = run_query(&db, "PRAGMA schema_version");
    assert!(schema_version.trim().parse::<u64>().is_ok());
}

/// `journal_mode` (#388) is a write pragma with no result set — typing
/// it through `query` (rather than `exec`) must still surface a clear
/// error, not be silently swallowed by the new pragma-recognizer path.
#[test]
fn unrecognized_pragma_falls_through_to_existing_error() {
    let db = seed_db("fallthrough", &[]);
    let output = Command::new(CLI)
        .arg("query")
        .arg(&db)
        .arg("PRAGMA journal_mode")
        .output()
        .unwrap();
    assert!(!output.status.success());
}

/// Best-effort cross-check against a real `sqlite3` when one matching
/// the pinned oracle version is available (CI/dev machines commonly
/// won't have that exact pin, so this degrades gracefully rather than
/// failing) — `table_info`'s column order/semantics is the one this
/// ticket calls out explicitly for oracle validation.
#[test]
fn table_info_matches_oracle_when_available() {
    let Some(oracle) = pinned_oracle() else {
        eprintln!("skipping table_info_matches_oracle_when_available: no pinned oracle sqlite3");
        return;
    };
    let db = seed_db(
        "oracle-table-info",
        &["CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT NOT NULL DEFAULT 'x', c REAL)"],
    );
    let oracle_out = Command::new(&oracle)
        .arg(&db)
        .arg("PRAGMA table_info(t);")
        .output()
        .unwrap();
    assert!(oracle_out.status.success());
    let oracle_rows = String::from_utf8_lossy(&oracle_out.stdout).replace(',', "|");
    let ours = run_query(&db, "PRAGMA table_info(t)");
    assert_eq!(ours, oracle_rows);
}
