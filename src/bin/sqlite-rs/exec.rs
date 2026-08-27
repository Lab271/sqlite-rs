// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! `exec <file> "<SQL>"`: runs one or more `;`-separated statements
//! (INSERT/UPDATE/DELETE/CREATE TABLE/DROP TABLE/CREATE INDEX/DROP
//! INDEX/BEGIN/COMMIT/ROLLBACK) against a writable `Pager` (#215's
//! write-path CLI surface, extended by #358/#360 to a multi-statement
//! session: `BEGIN`/a write/`COMMIT` each compile to their own
//! `Program`, so running a script needs one shared `Pager` and
//! autocommit flag threaded across them —
//! [`sqlite_rs::vdbe::execute_transaction_step`] is exactly that).
//! Matches stock `sqlite3`'s CLI behavior of printing nothing on success
//! for a bare DML/DDL statement (no `.echo`/`-changes` flag requested).
//!
//! Not a REPL (an explicit non-goal, see the module doc on `main.rs`):
//! one process invocation, one `-e`-style script string, exactly like
//! `sqlite3 <file> "<sql>"` itself. Stops at the first failing
//! statement rather than continuing past it — simpler than stock
//! `sqlite3`'s default (continue past errors, exit non-zero if any
//! failed), and fine for a scripted/CI caller that wants to know
//! exactly where a script broke.

use std::cell::RefCell;
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_statement;
use sqlite_rs::dump;
use sqlite_rs::header::{DatabaseHeader, DEFAULT_PAGE_SIZE};
use sqlite_rs::parser::split_statements;
use sqlite_rs::schema::{read_schema_and_views, TableSchema, ViewSchema};
use sqlite_rs::vdbe::execute_transaction_step;
use sqlite_rs::vfs::{UnixVfs, Vfs};

use crate::common::fatal;

pub fn run_exec(path: &Path, sql: &str) -> ExitCode {
    // Unlike `query`/`dump`/`tables`, `exec` is a bootstrap surface: a
    // script's first statement may be the `CREATE TABLE` that gives the
    // target file its very first byte, matching stock `sqlite3 <file>
    // "<sql>"`'s lazy file creation. Write a valid empty-database page 1
    // up front so `dump::open` below has a header it can actually parse.
    if !matches!(UnixVfs.exists(path), Ok(true)) {
        let file = match UnixVfs.create_or_open_write(path) {
            Ok(f) => f,
            Err(e) => return fatal(path, &e),
        };
        let page1 = DatabaseHeader::new_empty_page1(DEFAULT_PAGE_SIZE);
        if let Err(e) = file.write_at(&page1, 0) {
            return fatal(path, &e);
        }
    }

    let (header, pager) = match dump::open(&UnixVfs, path) {
        Ok(v) => v,
        Err(e) => return fatal(path, &e),
    };
    let pager = Rc::new(RefCell::new(pager));

    let mut autocommit = true;
    // The decoded catalog, reused across statements (#589): re-reading
    // `sqlite_master` before every statement (the pre-#589 behavior) is
    // two b-tree walks per statement even for a script of pure SELECTs.
    // The cache is dropped after any statement that can change the
    // schema, so a script that `CREATE TABLE`s and then writes to that
    // same table still sees a catalog reflecting what already ran.
    let mut catalog: Option<(Vec<TableSchema>, Vec<ViewSchema>)> = None;
    for stmt in split_statements(sql) {
        if catalog.is_none() {
            let borrowed = pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
            match read_schema_and_views(&mut schema_cursor, header.text_encoding) {
                Ok(pair) => catalog = Some(pair),
                Err(e) => return fatal(path, &e),
            }
        }
        let Some((schemas, views)) = catalog.as_ref() else {
            // Unreachable: the block above always fills the cache.
            return ExitCode::FAILURE;
        };

        let program = match compile_statement(&stmt, schemas, views) {
            Ok(p) => p,
            Err(e) => return fatal(path, &e),
        };

        match execute_transaction_step(&program, Rc::clone(&pager), header, autocommit) {
            Ok((_, ac)) => autocommit = ac,
            Err(e) => return fatal(path, &e),
        }

        if is_schema_changing(&stmt) {
            catalog = None;
        }
    }

    ExitCode::SUCCESS
}

/// Whether `stmt` can change the `sqlite_master` catalog — the DDL
/// dirty flag for the catalog cache above. Deliberately conservative:
/// any statement starting with `CREATE`/`DROP`/`ALTER` invalidates,
/// even one that ends up failing or being a no-op.
fn is_schema_changing(stmt: &str) -> bool {
    let head = stmt.trim_start();
    ["CREATE", "DROP", "ALTER"].iter().any(|kw| {
        head.get(..kw.len())
            .is_some_and(|h| h.eq_ignore_ascii_case(kw))
    })
}
