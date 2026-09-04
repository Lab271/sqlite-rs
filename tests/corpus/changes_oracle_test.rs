// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Oracle diff for the rows-changed counter (spec 013 Requirement 1,
//! #692): runs one statement sequence through this crate's write path and
//! the same sequence through the pinned `sqlite3`, and compares the count
//! each `INSERT`/`UPDATE`/`DELETE` reports.
//!
//! The unit suite (`tests/unit/vdbe_changes_test.rs`) pins the mechanism —
//! that `OPFLAG_NCHANGE` is what counts, so an `UPDATE`'s `Delete` +
//! `Insert` pair is one change and index maintenance is none. This pins
//! the *answers* against the definition of correctness, which is SQLite.

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

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_statement;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::schema::{read_schema, read_views};
use sqlite_rs::vdbe::execute_transaction_step_counted;
use sqlite_rs::vfs::MemoryVfs;

use crate::oracle::{pinned_oracle, skip_no_oracle};

/// The sequence both engines run. Every statement is one this crate
/// compiles today, and the `UPDATE`/`DELETE` predicates are chosen to
/// cover a match, a partial match and a miss.
const STATEMENTS: &[&str] = &[
    "CREATE TABLE t(a INTEGER, b TEXT, c TEXT)",
    "CREATE INDEX t_a ON t(a)",
    "CREATE UNIQUE INDEX t_c ON t(c)",
    "INSERT INTO t VALUES (1, 'b1', 'c1')",
    "INSERT INTO t VALUES (2, 'b2', 'c2')",
    "INSERT INTO t VALUES (3, 'b3', 'c3')",
    "INSERT INTO t VALUES (4, 'b4', 'c4')",
    // Touches the scanned index -> two-pass plan (#675).
    "UPDATE t SET a = a + 10 WHERE a > 2",
    // Leaves it alone -> single-pass plan.
    "UPDATE t SET b = 'z' WHERE a > 2",
    // Matches nothing.
    "UPDATE t SET b = 'q' WHERE a = 999",
    "DELETE FROM t WHERE a < 3",
    "DELETE FROM t",
    "DELETE FROM t",
];

fn is_dml(sql: &str) -> bool {
    let head = sql.trim_start();
    ["INSERT", "UPDATE", "DELETE"]
        .iter()
        .any(|kw| head.len() >= kw.len() && head[..kw.len()].eq_ignore_ascii_case(kw))
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

/// Runs `STATEMENTS` through this crate, returning the count each DML
/// statement reported.
fn ours() -> Vec<(&'static str, Option<u64>)> {
    let page_size = 4096;
    let (vfs, header) = empty_db(page_size);
    let pager = Rc::new(RefCell::new(
        Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap(),
    ));
    let mut autocommit = true;
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
        let program = compile_statement(sql, &schemas, &views)
            .unwrap_or_else(|e| panic!("{sql} did not compile: {e}"));
        let outcome =
            execute_transaction_step_counted(&program, Rc::clone(&pager), header, autocommit)
                .unwrap_or_else(|e| panic!("{sql} failed: {e}"));
        autocommit = outcome.autocommit;
        out.push((*sql, outcome.changes));
    }
    out
}

/// Runs `STATEMENTS` through the pinned oracle, returning `changes()`
/// after each DML statement.
///
/// One `sqlite3` invocation per statement, with `SELECT changes()`
/// appended: `changes()` is per-connection state, and a fresh invocation
/// starts it at zero, so asking inside the same invocation as the
/// statement is what reports that statement's own count.
fn oracle(bin: &Path, db: &Path) -> Vec<(&'static str, Option<u64>)> {
    let mut out = Vec::new();
    for sql in STATEMENTS {
        let script = if is_dml(sql) {
            format!("{sql};\nSELECT changes();")
        } else {
            format!("{sql};")
        };
        let output = Command::new(bin)
            .arg(db)
            .arg(&script)
            .output()
            .unwrap_or_else(|e| panic!("oracle failed to run {sql}: {e}"));
        assert!(
            output.status.success(),
            "oracle rejected {sql}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let changed = if is_dml(sql) {
            Some(
                String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .parse::<u64>()
                    .unwrap_or_else(|e| {
                        panic!(
                            "oracle's changes() after {sql} was not a number ({e}): {:?}",
                            String::from_utf8_lossy(&output.stdout)
                        )
                    }),
            )
        } else {
            None
        };
        out.push((*sql, changed));
    }
    out
}

#[test]
fn rows_changed_counts_match_the_oracle() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("rows_changed_counts_match_the_oracle");
        return;
    };
    let dir = tempdir();
    let db = dir.join("changes.db");

    let mine = ours();
    let theirs = oracle(&bin, &db);

    // Compare only the DML statements: `changes()` is undefined-by-design
    // for a DDL statement (the oracle reports whatever the connection's
    // previous count was; we report `None`), so the interesting claim is
    // the counting statements agreeing exactly.
    let mine_dml: Vec<_> = mine.iter().filter(|(sql, _)| is_dml(sql)).collect();
    let theirs_dml: Vec<_> = theirs.iter().filter(|(sql, _)| is_dml(sql)).collect();

    assert_eq!(mine_dml, theirs_dml, "rows-changed counts diverge");

    // And the DDL half really is `None` on our side rather than an
    // accidental `Some(0)`, which is the distinction a connection needs
    // in order not to clobber a retained count.
    for (sql, changed) in &mine {
        if !is_dml(sql) {
            assert_eq!(*changed, None, "{sql} claimed a rows-changed count");
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

fn tempdir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sqlite-rs-changes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
