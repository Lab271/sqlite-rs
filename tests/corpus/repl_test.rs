//! End-to-end tests of the `sqlite-rs repl` CLI subcommand (#365): a
//! minimal read-eval-print loop, driven here by piping a script into
//! its stdin — exactly the acceptance transcript from the issue body
//! (`BEGIN; INSERT ...; SELECT ...; ROLLBACK; SELECT ...; .quit`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("sqlite-rs-repl-{label}-{}-{n}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

/// A fresh scratch db with `t(a INTEGER)` — via the pinned oracle if
/// available, else via `sqlite-rs exec` itself.
fn seed_db(label: &str) -> PathBuf {
    let db = scratch_db(label);
    if let Some(oracle) = pinned_oracle() {
        let status = Command::new(&oracle)
            .arg(&db)
            .arg("CREATE TABLE t(a INTEGER)")
            .status()
            .unwrap();
        assert!(status.success());
    } else {
        let status = Command::new(CLI)
            .arg("exec")
            .arg(&db)
            .arg("CREATE TABLE t(a INTEGER)")
            .status()
            .unwrap();
        assert!(status.success());
    }
    db
}

/// Pipes `script` (one REPL line per element) into `repl <db>`'s stdin
/// and returns the completed process's output.
fn run_repl(db: &Path, script: &[&str]) -> Output {
    let mut child = Command::new(CLI)
        .arg("repl")
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawning {CLI} repl {}: {e}", db.display()));

    {
        let stdin = child.stdin.as_mut().unwrap();
        for line in script {
            writeln!(stdin, "{line}").unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

/// Strips every `sqlite> `/`   ...> ` prompt the REPL printed, so a
/// test can assert on just the rows/errors it actually emitted.
fn strip_prompts(stdout: &str) -> String {
    stdout.replace("sqlite> ", "").replace("   ...> ", "")
}

/// The issue's own acceptance transcript: a write inside an open
/// transaction is visible to a `SELECT` on the same connection, and
/// disappears after `ROLLBACK`.
#[test]
fn begin_insert_select_rollback_select_matches_the_issue_transcript() {
    let db = seed_db("acceptance");
    let output = run_repl(
        &db,
        &[
            "BEGIN;",
            "INSERT INTO t VALUES (1);",
            "SELECT * FROM t;",
            "ROLLBACK;",
            "SELECT * FROM t;",
            ".quit",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = strip_prompts(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(stdout, "1\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    } else {
        skip_no_oracle("begin_insert_select_rollback_select_matches_the_issue_transcript");
    }
}

/// Same shape, but `COMMIT` — the write must persist past the session
/// (verified by reopening through a fresh `exec`/`query` invocation,
/// not just within the same REPL process).
#[test]
fn begin_insert_commit_persists_past_the_session() {
    let db = seed_db("commit");
    let output = run_repl(
        &db,
        &["BEGIN;", "INSERT INTO t VALUES (7);", "COMMIT;", ".quit"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let query = Command::new(CLI)
        .arg("query")
        .arg(&db)
        .arg("SELECT * FROM t")
        .output()
        .unwrap();
    assert!(query.status.success());
    assert_eq!(String::from_utf8_lossy(&query.stdout), "7\n");

    if let Some(oracle) = pinned_oracle() {
        assert_integrity_check_ok(&oracle, &db);
    } else {
        skip_no_oracle("begin_insert_commit_persists_past_the_session");
    }
}

/// A statement typed across multiple lines (no trailing `;` yet) must
/// not run until the terminating `;` arrives — and a semicolon inside
/// a string literal must not end the statement early either.
#[test]
fn multi_line_statement_and_semicolon_in_string_literal() {
    let db = seed_db("multiline");
    let output = run_repl(
        &db,
        &["INSERT INTO t", "VALUES (1);", "SELECT 'a;b';", ".quit"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = strip_prompts(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(stdout, "a;b\n");
}

/// A bad statement reports an error but does not end the session —
/// the next statement still runs, matching `sqlite3`'s own shell.
#[test]
fn bad_statement_reports_error_and_session_continues() {
    let db = seed_db("bad_stmt");
    let output = run_repl(&db, &["BOGUS;", "SELECT 1;", ".quit"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).is_empty());
    let stdout = strip_prompts(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(stdout, "1\n");
}

/// Nested `BEGIN` and a bare `COMMIT` both report an error (matching
/// stock sqlite3's "cannot start a transaction within a transaction" /
/// "cannot commit - no transaction is active") rather than silently
/// succeeding, and the session continues afterward (#396).
#[test]
fn nested_begin_and_bare_commit_report_errors_and_session_continues() {
    let db = seed_db("txn_state_errors");
    let output = run_repl(
        &db,
        &[
            "BEGIN;",
            "BEGIN;",
            "ROLLBACK;",
            "COMMIT;",
            "SELECT 1;",
            ".quit",
        ],
    );
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot start a transaction within a transaction"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("cannot commit - no transaction is active"),
        "stderr: {stderr}"
    );
    let stdout = strip_prompts(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(stdout, "1\n");
}

/// `.exit` behaves the same as `.quit`.
#[test]
fn dot_exit_also_ends_the_session() {
    let db = seed_db("dot_exit");
    let output = run_repl(&db, &["SELECT 1;", ".exit"]);
    assert!(output.status.success());
    let stdout = strip_prompts(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(stdout, "1\n");
}

/// Opening a nonexistent path reports an error and exits with failure,
/// rather than panicking.
#[test]
fn opening_a_bad_path_reports_an_error_and_fails() {
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-repl-bad-path-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::remove_dir_all(&dir).ok();
    let missing = dir.join("does-not-exist.db");
    let output = run_repl(&missing, &[".quit"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).is_empty());
}

/// An unrecognized dot-command reports an error but the session
/// continues (the REPL's own explicit scope-down: no dot-command
/// beyond `.quit`/`.exit`).
#[test]
fn unknown_dot_command_reports_error_and_session_continues() {
    let db = seed_db("unknown_dot_command");
    let output = run_repl(&db, &[".tables", "SELECT 1;", ".quit"]);
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown command"), "stderr: {stderr}");
    let stdout = strip_prompts(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(stdout, "1\n");
}

/// Closing stdin without a `.quit`/`.exit` (e.g. a piped script that
/// just ends) ends the session cleanly at EOF, same as an explicit
/// `.quit`.
#[test]
fn stdin_eof_without_dot_quit_ends_the_session() {
    let db = seed_db("eof_no_quit");
    let output = run_repl(&db, &["SELECT 1;"]);
    assert!(output.status.success());
    let stdout = strip_prompts(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(stdout, "1\n");
}

/// A `SELECT` using syntactically-recognized-but-unimplemented syntax
/// reports a "not yet supported" error and the session continues.
#[test]
fn select_with_unsupported_syntax_reports_error_and_session_continues() {
    let db = seed_db("unsupported_select");
    let output = run_repl(&db, &["SELECT * FROM t JOIN t;", "SELECT 1;", ".quit"]);
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not yet supported"), "stderr: {stderr}");
    let stdout = strip_prompts(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(stdout, "1\n");
}

/// A `SELECT` with a genuine syntax error reports a "syntax error"
/// diagnostic and the session continues.
#[test]
fn select_with_syntax_error_reports_error_and_session_continues() {
    let db = seed_db("select_syntax_error");
    let output = run_repl(&db, &["SELECT FROM;", "SELECT 1;", ".quit"]);
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("syntax error"), "stderr: {stderr}");
    let stdout = strip_prompts(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(stdout, "1\n");
}

/// A `SELECT` that parses fine but fails to compile (e.g. a missing
/// table) reports the compile error and the session continues.
#[test]
fn select_from_missing_table_reports_error_and_session_continues() {
    let db = seed_db("select_missing_table");
    let output = run_repl(&db, &["SELECT * FROM nope;", "SELECT 1;", ".quit"]);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).is_empty());
    let stdout = strip_prompts(&String::from_utf8_lossy(&output.stdout));
    assert_eq!(stdout, "1\n");
}
