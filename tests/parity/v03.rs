//! V03 parity: the write path (#161's INSERT/UPDATE/DELETE +
//! CREATE/DROP TABLE/INDEX surface, exposed via the `sqlite-rs exec`
//! CLI subcommand, #215). Same five-dimension vocabulary as `v01.rs`/
//! `v02.rs`, adapted for statements that mutate a database rather than
//! only read one: each case seeds an independent scratch copy per
//! engine, replays the same statement sequence against both via their
//! own CLI (`sqlite-rs exec` / `sqlite3`), then compares final state —
//! `SELECT` output for DML cases, `sqlite_master` listing for DDL
//! cases. VM-instruction dimension doesn't apply to a CLI-level
//! comparison and is recorded as skipped, same discipline as `v02.rs`.
//!
//! See issue #72.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::driver::DimResult;
use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-parity-v03-{label}-{}-{n}",
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

/// `sqlite-rs exec` opens its target through the same read-then-parse
/// path `dump`/`query` use (`dump::open`), which requires an existing,
/// already-valid database file — it never creates one. A brand-new
/// scratch path has no such file yet: `sqlite3` itself is lazy too (a
/// read-only `PRAGMA` never materializes a file on disk), so this
/// forces a real write — create-then-drop a throwaway table — to
/// establish a valid on-disk header before either engine's `exec`
/// touches the file, without leaving any trace in `sqlite_master`.
fn bootstrap(oracle: &Path, db: &Path) {
    let status = Command::new(oracle)
        .arg(db)
        .arg("CREATE TABLE bootstrap_scratch(x); DROP TABLE bootstrap_scratch;")
        .status()
        .unwrap_or_else(|e| panic!("bootstrapping {}: {e}", db.display()));
    assert!(status.success(), "bootstrapping {} failed", db.display());
}

/// One write-path case: a setup statement (creates the table(s) the
/// case operates on, run identically on both engines), a sequence of
/// statements under test, and a final read used to compare state.
struct WriteCase {
    name: &'static str,
    setup: &'static [&'static str],
    statements: &'static [&'static str],
    verify: &'static str,
}

/// Runs every statement in `case` against a fresh scratch db for each
/// engine, then compares: acceptance (did every statement succeed on
/// both sides, or fail on both), and output (does `verify`'s result
/// match). Returns `None` (skip) if no pinned oracle is configured.
fn run_write_case(case: &WriteCase) -> Option<(DimResult, DimResult)> {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle(case.name);
        return None;
    };

    let ours_db = scratch_db(&format!("{}-ours", case.name));
    let theirs_db = scratch_db(&format!("{}-theirs", case.name));
    bootstrap(&oracle, &ours_db);
    bootstrap(&oracle, &theirs_db);

    for stmt in case.setup.iter().chain(case.statements.iter()) {
        let ours_ok = ours_exec(&ours_db, stmt).status.success();
        let theirs_ok = oracle_exec(&oracle, &theirs_db, stmt).status.success();
        if ours_ok != theirs_ok {
            return Some((
                DimResult::Mismatch {
                    ours: format!("{stmt:?} -> success={ours_ok}"),
                    theirs: format!("{stmt:?} -> success={theirs_ok}"),
                },
                DimResult::Skipped("acceptance mismatch"),
            ));
        }
        if !ours_ok {
            // Both sides rejected the same statement (e.g. a UNIQUE
            // violation under ON CONFLICT ABORT) — acceptance agrees;
            // there's no further state to compare for this case.
            return Some((DimResult::Match, DimResult::Skipped("statement rejected")));
        }
    }

    let ours = ours_query(&ours_db, case.verify);
    let theirs = oracle_query(&oracle, &theirs_db, case.verify);
    let output = match (ours, theirs) {
        (Ok(a), Ok(b)) if a == b => DimResult::Match,
        (Ok(a), Ok(b)) => DimResult::Mismatch { ours: a, theirs: b },
        (a, b) => DimResult::Mismatch {
            ours: a.unwrap_or_else(|e| e),
            theirs: b.unwrap_or_else(|e| e),
        },
    };
    Some((DimResult::Match, output))
}

const CASES: &[WriteCase] = &[
    WriteCase {
        name: "basic_insert",
        setup: &["CREATE TABLE t(a INTEGER, b TEXT)"],
        statements: &["INSERT INTO t VALUES (1,'x'),(2,'y')"],
        verify: "SELECT * FROM t",
    },
    WriteCase {
        name: "update_and_delete",
        setup: &["CREATE TABLE t(a INTEGER, b TEXT)"],
        statements: &[
            "INSERT INTO t VALUES (1,'x'),(2,'y'),(3,'z')",
            "UPDATE t SET b = 'w' WHERE a = 2",
            "DELETE FROM t WHERE a = 3",
        ],
        verify: "SELECT * FROM t",
    },
    WriteCase {
        name: "insert_select",
        setup: &[
            "CREATE TABLE src(a INTEGER, b TEXT)",
            "CREATE TABLE t(a INTEGER, b TEXT)",
            "INSERT INTO src VALUES (1,'p'),(2,'q')",
        ],
        statements: &["INSERT INTO t SELECT * FROM src"],
        verify: "SELECT * FROM t",
    },
    WriteCase {
        name: "on_conflict_ignore",
        setup: &[
            "CREATE TABLE u(id INTEGER PRIMARY KEY, v TEXT)",
            "CREATE UNIQUE INDEX idx_u_v ON u(v)",
        ],
        statements: &[
            "INSERT INTO u VALUES (1,'a')",
            "INSERT OR IGNORE INTO u VALUES (2,'a')",
        ],
        verify: "SELECT * FROM u",
    },
    WriteCase {
        name: "on_conflict_replace",
        setup: &[
            "CREATE TABLE u(id INTEGER PRIMARY KEY, v TEXT)",
            "CREATE UNIQUE INDEX idx_u_v ON u(v)",
        ],
        statements: &[
            "INSERT INTO u VALUES (1,'a')",
            "INSERT OR REPLACE INTO u VALUES (2,'a')",
        ],
        verify: "SELECT * FROM u",
    },
    WriteCase {
        name: "unique_violation_is_rejected_on_both_engines",
        setup: &[
            "CREATE TABLE u(id INTEGER PRIMARY KEY, v TEXT)",
            "CREATE UNIQUE INDEX idx_u_v ON u(v)",
        ],
        statements: &[
            "INSERT INTO u VALUES (1,'a')",
            "INSERT INTO u VALUES (2,'a')",
        ],
        verify: "SELECT * FROM u",
    },
];

#[test]
fn acceptance_and_output_match_for_write_statements() {
    if pinned_oracle().is_none() {
        skip_no_oracle("acceptance_and_output_match_for_write_statements");
        return;
    }
    let mut checked = 0usize;
    for case in CASES {
        let Some((acceptance, output)) = run_write_case(case) else {
            continue;
        };
        assert_eq!(
            acceptance,
            DimResult::Match,
            "acceptance dimension mismatch for case {:?}",
            case.name
        );
        if output != DimResult::Skipped("statement rejected") {
            assert_eq!(
                output,
                DimResult::Match,
                "output dimension mismatch for case {:?}",
                case.name
            );
        }
        checked += 1;
    }
    assert!(
        checked > 0,
        "expected at least one write case to have been compared"
    );
}

/// Schema dimension: `CREATE TABLE`/`DROP TABLE`/`CREATE INDEX`/
/// `DROP INDEX` leave `sqlite_master` in matching states on both
/// engines.
#[test]
fn schema_table_names_match_after_ddl_statements() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("schema_table_names_match_after_ddl_statements");
        return;
    };

    let ours_db = scratch_db("ddl-ours");
    let theirs_db = scratch_db("ddl-theirs");
    bootstrap(&oracle, &ours_db);
    bootstrap(&oracle, &theirs_db);

    let statements = [
        "CREATE TABLE t(a INTEGER, b TEXT)",
        "CREATE INDEX idx_b ON t(b)",
        "CREATE TABLE scratch_table(x)",
        "DROP TABLE scratch_table",
        "DROP INDEX idx_b",
    ];
    for stmt in statements {
        assert!(
            ours_exec(&ours_db, stmt).status.success(),
            "our exec failed for {stmt:?}"
        );
        assert!(
            oracle_exec(&oracle, &theirs_db, stmt).status.success(),
            "oracle exec failed for {stmt:?}"
        );
    }

    // `query`'s FROM-clause resolution only knows about user tables (via
    // `read_schema`), not `sqlite_master` itself — reuse the `tables` CLI
    // subcommand (#177), which already lists `sqlite_master` table names
    // directly, excluding `sqlite_%` internals the same way this
    // comparison needs. `tables` now renders `sqlite3 .tables`'s
    // multi-column, space-padded layout rather than one name per line
    // (#177), so both sides are normalized to a sorted name list before
    // comparing rather than compared byte-for-byte.
    let ours = Command::new(CLI)
        .arg("tables")
        .arg(&ours_db)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} tables {}: {e}", ours_db.display()));
    assert!(ours.status.success(), "our `tables` subcommand failed");
    let ours_stdout = String::from_utf8_lossy(&ours.stdout).into_owned();
    let mut ours_names: Vec<&str> = ours_stdout.split_whitespace().collect();
    ours_names.sort_unstable();

    let names_sql =
        "select name from sqlite_schema where type='table' and name not like 'sqlite\\_%' escape '\\' order by name";
    let theirs =
        oracle_query(&oracle, &theirs_db, names_sql).expect("query sqlite_schema (oracle)");
    let mut theirs_names: Vec<&str> = theirs.lines().collect();
    theirs_names.sort_unstable();
    assert_eq!(
        ours_names, theirs_names,
        "schema dimension mismatch after DDL"
    );
}
