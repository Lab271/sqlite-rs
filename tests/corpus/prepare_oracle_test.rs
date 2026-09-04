// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Oracle diff for the lifted `SELECT` pipeline (#695).
//!
//! `tests/unit/prepare_test.rs` pins the dispatch and the error type.
//! This pins the *answers*: the same queries, compiled through
//! `codegen::compile_select_program` from the library and executed here,
//! must produce byte-identical rows to the pinned `sqlite3` 3.53.4.
//!
//! It exists because the lift's whole claim is "same code, new home". I
//! checked four queries by hand while doing it, which is worth nothing
//! once the terminal closes — a refactor of the path every `sqlite-rs
//! query` takes deserves a check that runs in CI.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::HashMap;
use std::process::Command;
use std::rc::Rc;

use sqlite_rs::codegen::{compile_select_program, result_column_names, SelectOutcome};
use sqlite_rs::parser::error::ParseOutcome;
use sqlite_rs::parser::parse_select;
use sqlite_rs::record::Value;
use sqlite_rs::schema::{read_schema, read_views};
use sqlite_rs::vdbe::execute_with_db;
use sqlite_rs::vfs::PageSource;

use crate::oracle::{pinned_oracle, skip_no_oracle};

const SETUP: &[&str] = &[
    "CREATE TABLE t(a INTEGER, b TEXT)",
    "CREATE TABLE u(a INTEGER, c TEXT)",
    "CREATE INDEX t_a ON t(a)",
    "INSERT INTO t VALUES (1,'x'),(2,'y'),(3,'z')",
    "INSERT INTO u VALUES (1,'p'),(3,'q')",
];

/// One per `compile_select_program` dispatch arm, plus the shapes SQE's
/// own statement list uses (`UNION`, `LIMIT 1` probes).
const QUERIES: &[&str] = &[
    "SELECT 1 + 1",
    "SELECT a, b FROM t ORDER BY a",
    "SELECT count(*) FROM t",
    "SELECT a FROM t WHERE a > 1 ORDER BY a",
    "SELECT b FROM t WHERE a = 2",
    "SELECT t.a, u.c FROM t JOIN u ON t.a = u.a ORDER BY t.a",
    "SELECT a FROM t UNION ALL SELECT a FROM u ORDER BY a",
    "SELECT a FROM t UNION SELECT a FROM u ORDER BY a",
    "SELECT b FROM t WHERE a = 1 LIMIT 1",
    "SELECT a FROM t WHERE b = 'nope' LIMIT 1",
    "SELECT max(a), min(a) FROM t",
];

/// Renders a row the way the oracle's default `-list` mode does, so the
/// two are comparable as text.
fn render(row: &[Value]) -> String {
    row.iter()
        .map(|v| match v {
            Value::Null => String::new(),
            Value::Integer(i) => i.to_string(),
            Value::Real(r) => format!("{r}"),
            Value::Text(s) => s.to_string(),
            Value::Blob(b) => String::from_utf8_lossy(b).to_string(),
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[test]
fn lifted_pipeline_answers_match_the_oracle() {
    let Some(bin) = pinned_oracle() else {
        skip_no_oracle("lifted_pipeline_answers_match_the_oracle");
        return;
    };
    let dir = std::env::temp_dir().join(format!("sqlite-rs-prepare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("prepare.db");
    std::fs::remove_file(&db).ok();

    // The oracle builds the fixture, so we are reading a file stock
    // SQLite wrote — the adoption direction that matters.
    let status = Command::new(&bin)
        .arg(&db)
        .arg(SETUP.join(";\n"))
        .status()
        .unwrap();
    assert!(status.success(), "oracle setup failed");

    let (_vfs, pager, header) = open_readonly(&db);

    for sql in QUERIES {
        let theirs = Command::new(&bin).arg(&db).arg(sql).output().unwrap();
        assert!(
            theirs.status.success(),
            "oracle rejected {sql}: {}",
            String::from_utf8_lossy(&theirs.stderr)
        );
        let expected = String::from_utf8_lossy(&theirs.stdout)
            .trim_end()
            .to_string();

        let select = match parse_select(sql) {
            ParseOutcome::Accepted(s) => *s,
            other => panic!("{sql} did not parse: {other:?}"),
        };
        let (schemas, views) = {
            let mut c1 = sqlite_rs::btree::TableCursor::new(&*pager, &header, 1);
            let schemas = read_schema(&mut c1, header.text_encoding).unwrap();
            let mut c2 = sqlite_rs::btree::TableCursor::new(&*pager, &header, 1);
            let views = read_views(&mut c2, header.text_encoding).unwrap();
            (schemas, views)
        };
        let program =
            match compile_select_program(&select, false, &schemas, &views, &HashMap::new())
                .unwrap_or_else(|e| panic!("{sql} did not compile: {e}"))
            {
                SelectOutcome::Program(p) => p,
                SelectOutcome::Eqp(_) => panic!("{sql} unexpectedly produced EQP rows"),
            };

        // Column names come from the same function the CLI's headers do,
        // so a divergence there is a divergence for both.
        let names = result_column_names(&select, &schemas);
        assert!(!names.is_empty(), "{sql} derived no column names");

        let source: Rc<dyn PageSource> = Rc::clone(&pager) as Rc<dyn PageSource>;
        let rows = execute_with_db(&program, source, header)
            .unwrap_or_else(|e| panic!("{sql} failed to execute: {e}"));
        let ours = rows
            .iter()
            .map(|r| render(r))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(ours, expected, "rows diverge for {sql}");
    }

    std::fs::remove_dir_all(&dir).ok();
}

fn open_readonly(
    path: &std::path::Path,
) -> (
    sqlite_rs::vfs::UnixVfs,
    Rc<sqlite_rs::pager::Pager>,
    sqlite_rs::header::DatabaseHeader,
) {
    let vfs = sqlite_rs::vfs::UnixVfs;
    let (header, pager) = sqlite_rs::dump::open(&vfs, path).expect("open fixture");
    (vfs, Rc::new(pager), header)
}
