// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Placeholder counting and named-parameter rejection (spec 013
//! Requirement 3).
//!
//! An embedding consumer binds parameters, and needs two things this crate
//! did not offer:
//!
//! 1. **How many parameters a statement wants.** Without it, a caller
//!    cannot be told it supplied the wrong number, and a transposed or
//!    short argument list silently becomes NULLs.
//!    [`Program::param_count`] answers that.
//! 2. **A refusal for the forms that don't work.** `:name`/`@name`/`$name`
//!    parse but were never wired to a parameter index, and used to compile
//!    to a fresh (NULL-reading) register. `WHERE x = :name` therefore
//!    became `WHERE x = NULL`, matched no row, and reported no error — a
//!    silent wrong answer.
//!
//! ## Numbering matches stock SQLite
//!
//! Verified against the pinned 3.53.4 source rather than from memory
//! (`src/expr.c:1317` `sqlite3ExprAssignVarNumber`):
//!
//! * a bare `?` takes `x = ++pParse->nVar` (`expr.c:1331`) — the next free
//!   number, and
//! * `?nnn` takes `x = nnn` and raises the ceiling,
//!   `if( x>pParse->nVar ) pParse->nVar = x` (`expr.c:1356`).
//!
//! `sqlite3_bind_parameter_count()` returns that `nVar`, so the count is
//! the *largest index used*, not the number of distinct placeholders. Our
//! `RegAlloc::anonymous_param`/`numbered_param` (`src/codegen.rs:328`) are
//! the same two rules, and `param_count` reads the ceiling back off the
//! emitted `Variable` instructions.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{
    compile_select_program, compile_statement, DispatchError, PrepareError, SelectOutcome,
};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::error::ParseOutcome;
use sqlite_rs::parser::parse_select;
use sqlite_rs::planner::Stats;
use sqlite_rs::schema::{read_schema, read_views, TableSchema, ViewSchema};
use sqlite_rs::vdbe::{execute_transaction_step_counted, Program};
use sqlite_rs::vfs::MemoryVfs;

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

struct Db {
    pager: Rc<RefCell<Pager>>,
    header: DatabaseHeader,
    autocommit: bool,
}

impl Db {
    fn new() -> Self {
        let page_size = 4096;
        let (vfs, header) = empty_db(page_size);
        let pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();
        Self {
            pager: Rc::new(RefCell::new(pager)),
            header,
            autocommit: true,
        }
    }

    fn catalog(&self) -> (Vec<TableSchema>, Vec<ViewSchema>) {
        let borrowed = self.pager.borrow();
        let mut schema_cursor = TableCursor::new(&*borrowed, &self.header, 1);
        let schemas = read_schema(&mut schema_cursor, self.header.text_encoding).unwrap();
        let mut view_cursor = TableCursor::new(&*borrowed, &self.header, 1);
        let views = read_views(&mut view_cursor, self.header.text_encoding).unwrap();
        (schemas, views)
    }

    fn ddl(&mut self, sql: &str) {
        let (schemas, views) = self.catalog();
        let program = compile_statement(sql, &schemas, &views).unwrap();
        let outcome = execute_transaction_step_counted(
            &program,
            Rc::clone(&self.pager),
            self.header,
            self.autocommit,
        )
        .unwrap();
        self.autocommit = outcome.autocommit;
    }

    fn select(&self, sql: &str) -> Result<Program, PrepareError> {
        let (schemas, views) = self.catalog();
        let select = match parse_select(sql) {
            ParseOutcome::Accepted(select) => *select,
            ParseOutcome::Unsupported { message, .. } | ParseOutcome::Invalid { message, .. } => {
                panic!("{sql} did not parse: {message}")
            }
        };
        let stats: HashMap<String, Stats> = HashMap::new();
        match compile_select_program(&select, false, &schemas, &views, &stats)? {
            SelectOutcome::Program(p) => Ok(p),
            SelectOutcome::Eqp(_) => panic!("{sql} compiled to EXPLAIN QUERY PLAN"),
        }
    }

    fn write(&self, sql: &str) -> Result<Program, DispatchError> {
        let (schemas, views) = self.catalog();
        compile_statement(sql, &schemas, &views)
    }
}

fn db_with_table() -> Db {
    let mut db = Db::new();
    db.ddl("CREATE TABLE t(a INTEGER, b TEXT, c TEXT)");
    db
}

#[test]
fn a_statement_without_placeholders_wants_no_parameters() {
    let db = db_with_table();
    assert_eq!(db.select("SELECT a FROM t").unwrap().param_count(), 0);
    assert_eq!(
        db.write("INSERT INTO t VALUES (1, 'x', 'y')")
            .unwrap()
            .param_count(),
        0
    );
}

#[test]
fn anonymous_placeholders_are_numbered_in_order() {
    let db = db_with_table();
    assert_eq!(
        db.select("SELECT a FROM t WHERE a = ?")
            .unwrap()
            .param_count(),
        1
    );
    assert_eq!(
        db.select("SELECT a FROM t WHERE a = ? AND b = ?")
            .unwrap()
            .param_count(),
        2
    );
    assert_eq!(
        db.write("INSERT INTO t VALUES (?, ?, ?)")
            .unwrap()
            .param_count(),
        3
    );
}

/// `sqlite3_bind_parameter_count` returns the largest index, not the number
/// of distinct placeholders (`expr.c:1356`). So `?3` alone wants 3.
#[test]
fn numbered_placeholders_report_the_largest_index_not_the_count() {
    let db = db_with_table();
    assert_eq!(
        db.select("SELECT a FROM t WHERE a = ?3")
            .unwrap()
            .param_count(),
        3,
        "?3 alone should want 3 parameters, matching sqlite3_bind_parameter_count"
    );
}

/// A repeated `?NNN` is one parameter bound once and read twice — the
/// ceiling does not move.
#[test]
fn a_repeated_numbered_placeholder_is_still_one_parameter() {
    let db = db_with_table();
    assert_eq!(
        db.select("SELECT a FROM t WHERE a = ?1 OR b = ?1")
            .unwrap()
            .param_count(),
        1
    );
}

/// Mixed forms: a bare `?` after `?5` takes 6, because `++nVar` reads the
/// ceiling `?5` already raised (`expr.c:1331` and `expr.c:1356` together).
#[test]
fn a_bare_placeholder_after_a_numbered_one_continues_from_the_ceiling() {
    let db = db_with_table();
    assert_eq!(
        db.select("SELECT a FROM t WHERE a = ?5 AND b = ?")
            .unwrap()
            .param_count(),
        6,
        "a bare ? following ?5 should take index 6"
    );
}

/// The silent-wrong-answer fix. Each named form must be refused at compile
/// time rather than compiling to an always-NULL register.
#[test]
fn named_parameters_are_refused_rather_than_bound_to_null() {
    let db = db_with_table();
    for sql in [
        "SELECT a FROM t WHERE a = :name",
        "SELECT a FROM t WHERE a = @name",
        "SELECT a FROM t WHERE a = $name",
    ] {
        let err = db
            .select(sql)
            .expect_err("a named parameter should not compile");
        let message = err.to_string();
        assert!(
            message.contains("named parameter"),
            "{sql} was refused, but not for being a named parameter: {message}"
        );
        assert!(
            message.contains("bind by position"),
            "{sql}'s refusal should say what to do instead: {message}"
        );
    }
}

/// The refusal names the placeholder, sigil included, so a caller with a
/// large statement can find it.
#[test]
fn the_refusal_names_the_offending_placeholder() {
    let db = db_with_table();
    for (sql, expected) in [
        ("SELECT a FROM t WHERE a = :tenant", ":tenant"),
        ("SELECT a FROM t WHERE a = @tenant", "@tenant"),
        ("SELECT a FROM t WHERE a = $tenant", "$tenant"),
    ] {
        let message = db.select(sql).expect_err("should be refused").to_string();
        assert!(
            message.contains(expected),
            "{sql}'s refusal should name {expected}: {message}"
        );
    }
}

/// A named parameter anywhere in a write statement is refused too — the
/// rejection lives in expression compilation, so it covers every statement
/// kind that compiles an expression.
#[test]
fn named_parameters_are_refused_in_writes_as_well_as_reads() {
    let db = db_with_table();
    for sql in [
        "INSERT INTO t VALUES (:a, 'x', 'y')",
        "UPDATE t SET b = :b WHERE a = 1",
        "DELETE FROM t WHERE a = :a",
    ] {
        let message = db
            .write(sql)
            .expect_err("a named parameter should not compile")
            .to_string();
        assert!(
            message.contains("named parameter"),
            "{sql} should be refused for being a named parameter: {message}"
        );
    }
}

/// Positional placeholders still work in every statement kind — the
/// rejection above must not have caught the supported forms with it.
#[test]
fn positional_placeholders_still_compile_everywhere() {
    let db = db_with_table();
    assert_eq!(
        db.write("INSERT INTO t VALUES (?, ?, ?)")
            .unwrap()
            .param_count(),
        3
    );
    assert_eq!(
        db.write("UPDATE t SET b = ? WHERE a = ?")
            .unwrap()
            .param_count(),
        2
    );
    assert_eq!(
        db.write("DELETE FROM t WHERE a = ?").unwrap().param_count(),
        1
    );
}
