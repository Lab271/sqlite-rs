// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Oracle diff for the last-inserted-rowid signal (spec 013 Requirement 1):
//! runs one statement sequence through this crate's write path and the same
//! sequence through the pinned `sqlite3`, and compares what
//! `sqlite3_last_insert_rowid()` reports after *every* statement.
//!
//! The claim under test is deliberately the *retained* value, not the
//! per-statement one. `StepOutcome::last_insert_rowid` is `Option<i64>` —
//! `None` meaning "this statement inserted nothing, leave the stored value
//! alone" — and the interesting question is whether folding that signal the
//! way a connection will (`retained = reported.unwrap_or(retained)`)
//! reproduces SQLite's connection-scoped `db->lastRowid`. Comparing only the
//! statements that *do* insert would pass even if `UPDATE` wrongly cleared
//! the value, which is exactly the bug `OPFLAG_LASTROWID` exists to prevent.
//!
//! The unit suite (`tests/unit/vdbe_last_insert_rowid_test.rs`) pins the
//! mechanism; this pins the answers against the definition of correctness.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::cell::RefCell;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use std::collections::HashMap;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_select_program, compile_statement, SelectOutcome};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::{read_schema, read_views, TableSchema, ViewSchema};
use sqlite_rs::vdbe::{execute_transaction_step_counted, Program};
use sqlite_rs::vfs::MemoryVfs;

use crate::oracle::{pinned_oracle, skip_no_oracle};

/// The sequence both engines run.
///
/// Every statement is one this crate compiles today, and between them they
/// cover each way the value can move or must hold still:
///
/// * an implicit rowid from `NewRowid` (`t`),
/// * an *explicit* rowid supplied as a value for an `INTEGER PRIMARY KEY`
///   (`u`), where `NewRowid` never runs at all — the case that makes hooking
///   `Insert` rather than `NewRowid` load-bearing,
/// * a non-contiguous explicit rowid, so a stale-value bug cannot hide
///   behind a coincidence,
/// * `UPDATE` (matching and non-matching), `DELETE` and `SELECT`, none of
///   which may move the value,
/// * inserts into an indexed table, where index maintenance must not be
///   mistaken for a row insert.
const STATEMENTS: &[&str] = &[
    "CREATE TABLE t(a INTEGER, b TEXT)",
    "INSERT INTO t VALUES (1, 'b1')",
    "INSERT INTO t VALUES (2, 'b2')",
    // Must not move the value: an UPDATE rewrites a row as Delete+Insert,
    // and only the INSERT path carries OPFLAG_LASTROWID.
    "UPDATE t SET b = 'z' WHERE a = 1",
    "UPDATE t SET b = 'q' WHERE a = 999",
    // Must not move the value.
    "DELETE FROM t WHERE a = 1",
    "SELECT a FROM t",
    // Explicit rowid via INTEGER PRIMARY KEY: NewRowid never executes, so
    // the reported rowid has to come from the insert's own key.
    "CREATE TABLE u(id INTEGER PRIMARY KEY, v TEXT)",
    "INSERT INTO u(id, v) VALUES (42, 'forty-two')",
    "INSERT INTO u(id, v) VALUES (7, 'seven')",
    // Back to an implicit rowid on the other table — the value must follow
    // the most recent insert, not the highest rowid ever seen.
    "INSERT INTO t VALUES (3, 'b3')",
    // Index maintenance must not count as an insert.
    "CREATE INDEX u_v ON u(v)",
    "INSERT INTO u(id, v) VALUES (100, 'hundred')",
    "DELETE FROM u",
];

/// Compiles any statement in `STATEMENTS`, whichever kind it is.
///
/// `compile_statement` handles writes and DDL but answers
/// `Unrecognized("SELECT")` for a read, so a sequence containing both needs
/// the two entry points dispatched between — which is exactly the shape
/// spec 013's `Connection::prepare` has to present as one call, and why
/// #695 lifted the `SELECT` pipeline into the library. This helper is that
/// dispatch in miniature; the `SELECT` in the sequence is load-bearing
/// (a read must not move the value) so it cannot simply be dropped.
fn compile_any(sql: &str, schemas: &[TableSchema], views: &[ViewSchema]) -> Program {
    if !sql.trim_start().to_ascii_uppercase().starts_with("SELECT") {
        return compile_statement(sql, schemas, views)
            .unwrap_or_else(|e| panic!("{sql} did not compile: {e}"));
    }
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(select) => *select,
        ParseOutcome::Unsupported { message, .. } | ParseOutcome::Invalid { message, .. } => {
            panic!("{sql} did not parse: {message}")
        }
    };
    let stats: HashMap<String, sqlite_rs::planner::Stats> = HashMap::new();
    match compile_select_program(&select, false, schemas, views, &stats) {
        Ok(SelectOutcome::Program(program)) => program,
        Ok(SelectOutcome::Eqp(_)) => panic!("{sql} unexpectedly compiled to EXPLAIN QUERY PLAN"),
        Err(e) => panic!("{sql} did not compile: {e}"),
    }
}

fn empty_db(page_size: u32) -> (MemoryVfs, DatabaseHeader) {
    let mut page1 = vec![0u8; page_size as usize];
    page1[0..16].copy_from_slice(b"SQLite format 3\0");
    page1[16..18].copy_from_slice(&u16::try_from(page_size).unwrap().to_be_bytes());
    page1[18] = 1;
    page1[19] = 1;
    page1[28..32].copy_from_slice(&1u32.to_be_bytes());
    page1[56..60].copy_from_slice(&1u32.to_be_bytes());
    page1[100] = 0x0D;
    page1[105..107].copy_from_slice(&u16::try_from(page_size).unwrap().to_be_bytes());

    let mut header_bytes = [0u8; 100];
    header_bytes.copy_from_slice(&page1[..100]);
    let header = DatabaseHeader::parse(&header_bytes).unwrap();

    let mut vfs = MemoryVfs::new();
    vfs.insert("/test.db", page1);
    (vfs, header)
}

/// Runs `STATEMENTS` through this crate, folding the per-statement signal
/// into a connection-scoped value the way spec 013's `Connection` will, and
/// returning that retained value after each statement.
///
/// Also returns the raw per-statement signal so the caller can assert the
/// `None`-means-retain half directly rather than only through the fold.
fn ours() -> Vec<(&'static str, i64, Option<i64>)> {
    let page_size = 4096;
    let (vfs, header) = empty_db(page_size);
    let pager = Rc::new(RefCell::new(
        Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap(),
    ));
    let mut autocommit = true;
    // SQLite's `db->lastRowid` starts at 0 on a fresh connection.
    let mut retained: i64 = 0;
    let mut out = Vec::new();

    for sql in STATEMENTS {
        let (schemas, views) = {
            let borrowed = pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
            let schemas = read_schema(&mut schema_cursor, header.text_encoding).unwrap();
            let mut view_cursor = TableCursor::new(&*borrowed, &header, 1);
            let views = read_views(&mut view_cursor, header.text_encoding).unwrap();
            (schemas, views)
        };
        let program = compile_any(sql, &schemas, &views);
        let outcome =
            execute_transaction_step_counted(&program, Rc::clone(&pager), header, autocommit)
                .unwrap_or_else(|e| panic!("{sql} failed: {e}"));
        autocommit = outcome.autocommit;
        let reported = outcome.last_insert_rowid;
        retained = reported.unwrap_or(retained);
        out.push((*sql, retained, reported));
    }
    out
}

/// Runs `STATEMENTS` through the pinned oracle in a *single* invocation,
/// reading `last_insert_rowid()` after each one.
///
/// One invocation, unlike the rows-changed diff's one-per-statement: the
/// value under test is connection-scoped and retained across statements, so
/// a fresh connection per statement would reset it to 0 and make the
/// retention claim untestable. Results are tagged with a `LIR|` marker so
/// the probe output can be separated from the output of the statements
/// themselves (one of which is a `SELECT`).
fn oracle(bin: &Path, db: &Path) -> Vec<(&'static str, i64)> {
    let mut script = String::new();
    for sql in STATEMENTS {
        script.push_str(sql);
        script.push_str(";\nSELECT 'LIR', last_insert_rowid();\n");
    }

    let output = Command::new(bin)
        .arg(db)
        .arg(&script)
        .output()
        .unwrap_or_else(|e| panic!("oracle failed to run the script: {e}"));
    assert!(
        output.status.success(),
        "oracle rejected the script: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let values: Vec<i64> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("LIR|"))
        .map(|v| {
            v.trim().parse::<i64>().unwrap_or_else(|e| {
                panic!("oracle's last_insert_rowid() was not a number ({e}): {v:?}")
            })
        })
        .collect();

    assert_eq!(
        values.len(),
        STATEMENTS.len(),
        "expected one probe per statement, got {} for {} statements; stdout was {stdout:?}",
        values.len(),
        STATEMENTS.len()
    );

    STATEMENTS.iter().copied().zip(values).collect()
}

#[test]
fn last_insert_rowid_matches_the_oracle() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("last_insert_rowid_matches_the_oracle");
        return;
    };
    let dir = tempdir();
    let db = dir.join("last_rowid.db");

    let mine = ours();
    let theirs = oracle(&bin, &db);

    let mine_retained: Vec<(&str, i64)> = mine.iter().map(|(sql, r, _)| (*sql, *r)).collect();
    assert_eq!(
        mine_retained, theirs,
        "last_insert_rowid diverges from the oracle"
    );

    // The fold above could also be satisfied by reporting `Some(previous)`
    // for a non-inserting statement, which would be wrong in a way the
    // comparison cannot see: a connection would then have no way to tell
    // "nothing inserted" from "inserted the same rowid again". Assert the
    // signal itself is `None` for every statement that inserts no row.
    for (sql, _, reported) in &mine {
        let inserts = sql.trim_start().to_ascii_uppercase().starts_with("INSERT");
        if inserts {
            assert!(
                reported.is_some(),
                "{sql} inserted a row but reported no rowid"
            );
        } else {
            assert_eq!(
                *reported, None,
                "{sql} inserted nothing but claimed a last-insert rowid"
            );
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

fn tempdir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sqlite-rs-last-rowid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
