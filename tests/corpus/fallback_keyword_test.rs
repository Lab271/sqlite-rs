// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Oracle-diffed regression tests for #696: `parse.y:272`'s `%fallback
//! ID` list makes 89 of SQLite's 146 keywords non-reserved — they
//! double as ordinary identifiers (column/table/alias names) wherever a
//! keyword isn't expected. We used to reserve all 146 unconditionally,
//! which blocked schemas as ordinary as `PRIMARY KEY(namespace, key)`.
//!
//! The 89-word fallback set and the 57 still-reserved words below are
//! transcribed from the issue's own measurement against the pinned
//! 3.53.4 oracle (`src/parser/tokenizer.rs`'s `FALLBACK_KEYWORDS`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::oracle::{pinned_oracle, skip_no_oracle};

const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

/// The 89 keywords `parse.y:272` declares non-reserved (`%fallback ID`).
const FALLBACK_WORDS: &[&str] = &[
    "ABORT",
    "ACTION",
    "AFTER",
    "ALWAYS",
    "ANALYZE",
    "ASC",
    "ATTACH",
    "BEFORE",
    "BEGIN",
    "BY",
    "CASCADE",
    "CAST",
    "COLUMN",
    "CONFLICT",
    "CROSS",
    "CURRENT",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "DATABASE",
    "DEFERRED",
    "DESC",
    "DETACH",
    "DO",
    "EACH",
    "END",
    "EXCLUDE",
    "EXCLUSIVE",
    "EXPLAIN",
    "FAIL",
    "FILTER",
    "FIRST",
    "FOLLOWING",
    "FOR",
    "FULL",
    "GENERATED",
    "GLOB",
    "GROUPS",
    "IF",
    "IGNORE",
    "IMMEDIATE",
    "INDEXED",
    "INITIALLY",
    "INNER",
    "INSTEAD",
    "KEY",
    "LAST",
    "LEFT",
    "LIKE",
    "MATCH",
    "MATERIALIZED",
    "NATURAL",
    "NO",
    "NULLS",
    "OF",
    "OFFSET",
    "OTHERS",
    "OUTER",
    "OVER",
    "PARTITION",
    "PLAN",
    "PRAGMA",
    "PRECEDING",
    "QUERY",
    "RAISE",
    "RANGE",
    "RECURSIVE",
    "REGEXP",
    "REINDEX",
    "RELEASE",
    "RENAME",
    "REPLACE",
    "RESTRICT",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "SAVEPOINT",
    "TEMP",
    "TEMPORARY",
    "TIES",
    "TRIGGER",
    "UNBOUNDED",
    "VACUUM",
    "VIEW",
    "VIRTUAL",
    "WINDOW",
    "WITH",
    "WITHOUT",
];

/// The 57 keywords that stay fully reserved — not usable as a bare
/// (unquoted) column name.
const RESERVED_WORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "AUTOINCREMENT",
    "BETWEEN",
    "CASE",
    "CHECK",
    "COLLATE",
    "COMMIT",
    "CONSTRAINT",
    "CREATE",
    "DEFAULT",
    "DEFERRABLE",
    "DELETE",
    "DISTINCT",
    "DROP",
    "ELSE",
    "ESCAPE",
    "EXCEPT",
    "EXISTS",
    "FOREIGN",
    "FROM",
    "GROUP",
    "HAVING",
    "IN",
    "INDEX",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "ISNULL",
    "JOIN",
    "LIMIT",
    "NOT",
    "NOTHING",
    "NOTNULL",
    "ON",
    "OR",
    "ORDER",
    "PRIMARY",
    "REFERENCES",
    "RETURNING",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "TO",
    "TRANSACTION",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "WHEN",
    "WHERE",
];

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-fallback-kw-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

/// Runs `sql` against `db` through the pinned oracle `sqlite3` binary.
fn run(oracle: &Path, db: &Path, sql: &str) -> Output {
    Command::new(oracle)
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {} {} {sql:?}: {e}", oracle.display(), db.display()))
}

fn our_exec(db: &Path, sql: &str) -> Output {
    Command::new(CLI)
        .arg("exec")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} exec {} {sql:?}: {e}", db.display()))
}

#[test]
fn every_fallback_word_is_accepted_as_a_column_name_matching_the_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("every_fallback_word_is_accepted_as_a_column_name_matching_the_oracle");
        return;
    };
    for word in FALLBACK_WORDS {
        let sql = format!("CREATE TABLE t({word} TEXT)");
        let oracle_db = scratch_db(&format!("fallback-oracle-{word}"));
        let oracle_out = run(&oracle, &oracle_db, &sql);
        assert!(
            oracle_out.status.success(),
            "oracle unexpectedly rejected fallback word {word}: {}",
            String::from_utf8_lossy(&oracle_out.stderr)
        );

        let our_db = scratch_db(&format!("fallback-ours-{word}"));
        let our_out = our_exec(&our_db, &sql);
        assert!(
            our_out.status.success(),
            "we rejected fallback word {word} as a column name, oracle accepts it: {}",
            String::from_utf8_lossy(&our_out.stderr)
        );
    }
}

#[test]
fn every_reserved_word_is_rejected_as_a_bare_column_name_matching_the_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("every_reserved_word_is_rejected_as_a_bare_column_name_matching_the_oracle");
        return;
    };
    for word in RESERVED_WORDS {
        let sql = format!("CREATE TABLE t({word} TEXT)");
        let oracle_db = scratch_db(&format!("reserved-oracle-{word}"));
        let oracle_out = run(&oracle, &oracle_db, &sql);
        assert!(
            !oracle_out.status.success(),
            "oracle unexpectedly accepted reserved word {word} as a bare column name"
        );

        let our_db = scratch_db(&format!("reserved-ours-{word}"));
        let our_out = our_exec(&our_db, &sql);
        assert!(
            !our_out.status.success(),
            "we accepted reserved word {word} as a bare column name, oracle rejects it"
        );
    }
}

#[test]
fn sqe_namespace_properties_table_matches_the_oracle() {
    let sql_create =
        "CREATE TABLE p(namespace TEXT, key TEXT, value TEXT, PRIMARY KEY(namespace, key))";
    let sql_insert = "INSERT INTO p VALUES('ns', 'k', 'v')";
    let sql_select = "SELECT key, value FROM p";

    let our_db = scratch_db("sqe-ours");
    assert!(our_exec(&our_db, sql_create).status.success());
    assert!(our_exec(&our_db, sql_insert).status.success());
    let our_query = Command::new(CLI)
        .arg("query")
        .arg(&our_db)
        .arg(sql_select)
        .output()
        .unwrap();
    assert!(our_query.status.success());
    assert_eq!(String::from_utf8_lossy(&our_query.stdout), "k|v\n");

    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("sqe_namespace_properties_table_matches_the_oracle (oracle cross-check)");
        return;
    };
    let oracle_db = scratch_db("sqe-oracle");
    assert!(run(&oracle, &oracle_db, sql_create).status.success());
    assert!(run(&oracle, &oracle_db, sql_insert).status.success());
    let oracle_query = Command::new(&oracle)
        .arg(&oracle_db)
        .arg("-list")
        .arg(sql_select)
        .output()
        .unwrap();
    assert!(oracle_query.status.success());
    assert_eq!(String::from_utf8_lossy(&oracle_query.stdout), "k|v\n");
}

#[test]
fn a_fallback_word_works_as_both_keyword_and_identifier_in_one_statement() {
    // `PRIMARY KEY(key)` uses KEY as the reserved-position keyword and
    // as the column name in the same statement; `ORDER BY key` uses it
    // as both a bare column reference and a keyword-adjacent word.
    let our_db = scratch_db("both-in-one-ours");
    let create = "CREATE TABLE t(key TEXT, PRIMARY KEY(key))";
    assert!(our_exec(&our_db, create).status.success());
    assert!(our_exec(&our_db, "INSERT INTO t VALUES('a')")
        .status
        .success());
    let select = Command::new(CLI)
        .arg("query")
        .arg(&our_db)
        .arg("SELECT key FROM t ORDER BY key")
        .output()
        .unwrap();
    assert!(select.status.success(), "{:?}", select.stderr);
    assert_eq!(String::from_utf8_lossy(&select.stdout), "a\n");

    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("a_fallback_word_works_as_both_keyword_and_identifier_in_one_statement (oracle cross-check)");
        return;
    };
    let oracle_db = scratch_db("both-in-one-oracle");
    assert!(run(&oracle, &oracle_db, create).status.success());
    assert!(run(&oracle, &oracle_db, "INSERT INTO t VALUES('a')")
        .status
        .success());
    let oracle_select = Command::new(&oracle)
        .arg(&oracle_db)
        .arg("-list")
        .arg("SELECT key FROM t ORDER BY key")
        .output()
        .unwrap();
    assert!(oracle_select.status.success());
    assert_eq!(String::from_utf8_lossy(&oracle_select.stdout), "a\n");
}

#[test]
fn as_alias_using_a_fallback_word_parses() {
    // Regression guard for the `AS first` case the issue's Complexity
    // note cites as the trigger for filing #696.
    let db = scratch_db("as-alias");
    assert!(our_exec(&db, "CREATE TABLE t(a TEXT)").status.success());
    let out = Command::new(CLI)
        .arg("query")
        .arg(&db)
        .arg("SELECT a AS first FROM t")
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out.stderr);
}
