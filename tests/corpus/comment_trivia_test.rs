// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Oracle-diffed regression tests for #698: a SQL comment before or
//! after a statement must not be a parse error — comments are trivia,
//! not syntax, and the tokenizer already skips them everywhere except
//! at the statement-dispatch boundary (`codegen::dispatch`'s raw
//! keyword-sniffing, and `split_statements`'s comment-only-statement
//! handling).
//!
//! The oracle is invoked via stdin, not argv: a leading `--` in an argv
//! element is read as a CLI option by `sqlite3` itself (confirmed:
//! `sqlite3 db.db "-- c\nCREATE TABLE t(a)"` fails with `Error: unknown
//! option: - c` before it ever reaches the SQL parser), independent of
//! how the argument reached the process. `sqlite-rs exec`'s own
//! argument parsing is purely positional (`src/bin/sqlite-rs/main.rs`),
//! so it has no such restriction and is fed the same text via argv.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-comment-trivia-{label}-{}-{n}",
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

fn run_query(db: &Path, sql: &str) -> Output {
    Command::new(CLI)
        .arg("query")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} query {} {sql:?}: {e}", db.display()))
}

/// Runs `sql` against a fresh oracle-created `db` via stdin (never
/// argv — see the module doc for why), returning stdout as text.
fn oracle_via_stdin(oracle: &Path, db: &Path, sql: &str) -> String {
    let mut child = Command::new(oracle)
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawning oracle on {}: {e}", db.display()));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(sql.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "oracle rejected {sql:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Asserts our `exec` accepts `sql` and, when the pinned oracle is
/// available, that the two engines agree on the resulting stored DDL
/// text for `t` (`expected_ddl`, e.g. `"CREATE TABLE t(a)"` — a
/// statement's own leading/trailing comment must not leak into the
/// stored text, but a *mid*-statement comment, being part of the
/// statement's own span, must).
fn assert_accepted_and_matches_oracle(label: &str, sql: &str, expected_ddl: &str) {
    let db = scratch_db(label);
    let output = run_exec(&db, sql);
    assert!(
        output.status.success(),
        "{label}: our exec rejected {sql:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let our_schema = Command::new(CLI)
        .arg("dump")
        .arg(&db)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} dump {}: {e}", db.display()));
    assert!(our_schema.status.success());
    let our_schema = String::from_utf8_lossy(&our_schema.stdout).into_owned();
    assert!(
        our_schema.contains(expected_ddl),
        "our schema for {label}: {our_schema:?}"
    );

    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle(label);
        return;
    };
    let oracle_db = scratch_db(&format!("{label}-oracle"));
    let oracle_schema = oracle_via_stdin(&oracle, &oracle_db, &format!("{sql}\n.schema"));
    // `.schema`'s own trailing `;` differs from `dump`'s; compare only
    // the table's actual DDL text, which is what #698 is about (whether
    // a comment leaks into or blocks the statement), not dump-format
    // fidelity (covered by `dump_oracle_test.rs`).
    assert!(
        oracle_schema.contains(expected_ddl),
        "oracle schema for {label}: {oracle_schema:?}"
    );
}

#[test]
fn leading_line_comment_before_statement_is_accepted() {
    assert_accepted_and_matches_oracle(
        "leading-line",
        "-- c\nCREATE TABLE t(a);",
        "CREATE TABLE t(a)",
    );
}

#[test]
fn leading_block_comment_before_statement_is_accepted() {
    assert_accepted_and_matches_oracle(
        "leading-block",
        "/* c */ CREATE TABLE t(a);",
        "CREATE TABLE t(a)",
    );
}

#[test]
fn trailing_line_comment_after_statement_is_accepted() {
    assert_accepted_and_matches_oracle("trailing", "CREATE TABLE t(a); -- c", "CREATE TABLE t(a)");
}

#[test]
fn mid_statement_comment_still_works() {
    assert_accepted_and_matches_oracle(
        "mid-statement",
        "CREATE TABLE t(a /* col */);",
        "CREATE TABLE t(a /* col */)",
    );
}

#[test]
fn comment_only_input_is_a_successful_no_op() {
    let db = scratch_db("comment-only");
    let output = run_exec(&db, "-- just a comment");
    assert!(
        output.status.success(),
        "comment-only input should be a no-op, not an error: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    if let Some(oracle) = pinned_oracle() {
        let oracle_db = scratch_db("comment-only-oracle");
        // Succeeding at all (no error/panic from `wait_with_output`'s
        // status assertion) is the acceptance bar here.
        oracle_via_stdin(&oracle, &oracle_db, "-- just a comment");
    } else {
        skip_no_oracle("comment_only_input_is_a_successful_no_op (oracle cross-check)");
    }
}

#[test]
fn comment_like_text_inside_a_string_literal_stays_literal() {
    let db = scratch_db("string-literal");
    assert!(run_exec(&db, "CREATE TABLE t(a)").status.success());

    let output = run_query(&db, "SELECT '-- not a comment'");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "-- not a comment\n"
    );

    if let Some(oracle) = pinned_oracle() {
        let oracle_db = scratch_db("string-literal-oracle");
        let oracle_out = oracle_via_stdin(&oracle, &oracle_db, "SELECT '-- not a comment';");
        assert_eq!(oracle_out, "-- not a comment\n");
    } else {
        skip_no_oracle(
            "comment_like_text_inside_a_string_literal_stays_literal (oracle cross-check)",
        );
    }
}
