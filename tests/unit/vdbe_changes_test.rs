// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Rows-changed counter (spec 013 Requirement 1, #692).
//!
//! Spec 013 calls this the one item on its list a consumer cannot work
//! around: without it a caller cannot tell an `UPDATE` that matched from
//! one that did not, which is the distinction every
//! optimistic-concurrency scheme is built on.
//!
//! The counter is driven by `OPFLAG_NCHANGE` on `P5` rather than by the
//! opcode, and these tests are the reason. One `UPDATE`ed row emits a
//! `Delete` *and* an `Insert`, and the two-pass range-seek plan
//! (#666/#675) emits a third `Insert` against an ephemeral cursor to
//! stash matched rowids — so a handler that counted every
//! `Insert`/`Delete` would report 3 for a plan that changed 1 row, and 2
//! for the other plan of the same statement.
//! `update_of_one_row_reports_one_under_both_plans` pins exactly that.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_select_with_catalog, compile_statement};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::error::ParseOutcome;
use sqlite_rs::parser::parse_select;
use sqlite_rs::record::Value;
use sqlite_rs::schema::{read_schema, read_views, TableSchema, ViewSchema};
use sqlite_rs::vdbe::{execute_transaction_step_counted, Program, StepOutcome};
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

    fn step(&mut self, sql: &str) -> StepOutcome {
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
        outcome
    }

    /// Runs a statement that has no rows-changed count (DDL), asserting
    /// that it does not claim one.
    fn exec_ddl(&mut self, sql: &str) {
        assert_eq!(
            self.step(sql).changes,
            None,
            "{sql} should not be a counting statement"
        );
    }

    /// The rows-changed count for `sql`, asserting it is a counting
    /// statement at all.
    fn changes(&mut self, sql: &str) -> u64 {
        self.step(sql)
            .changes
            .unwrap_or_else(|| panic!("{sql} reported no rows-changed count"))
    }

    /// Compiles a write/DDL statement without running it.
    fn compile(&mut self, sql: &str) -> Program {
        let (schemas, views) = self.catalog();
        compile_statement(sql, &schemas, &views).unwrap()
    }

    /// Compiles a `SELECT`, which `compile_statement` does not handle.
    fn compile_select(&mut self, sql: &str) -> Program {
        let (schemas, _) = self.catalog();
        let select = match parse_select(sql) {
            ParseOutcome::Accepted(s) => s,
            other => panic!("{sql} did not parse: {other:?}"),
        };
        let schema = schemas
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case("t"))
            .unwrap();
        compile_select_with_catalog(&select, schema, &schemas).unwrap()
    }

    fn count_rows(&mut self, sql: &str) -> i64 {
        let program = self.compile_select(sql);
        let outcome = execute_transaction_step_counted(
            &program,
            Rc::clone(&self.pager),
            self.header,
            self.autocommit,
        )
        .unwrap();
        self.autocommit = outcome.autocommit;
        match &outcome.rows[0][0] {
            Value::Integer(n) => *n,
            other => panic!("expected an integer, got {other:?}"),
        }
    }
}

/// Spec 013/Req 1's first scenario: the same conditional `UPDATE` run
/// twice reports one row changed, then zero.
#[test]
fn conditional_update_reports_match() {
    let mut db = Db::new();
    db.exec_ddl("CREATE TABLE t(k TEXT, metadata_location TEXT)");
    db.changes("INSERT INTO t VALUES ('a', 'a')");

    let first = db.changes("UPDATE t SET metadata_location = 'b' WHERE metadata_location = 'a'");
    let second = db.changes("UPDATE t SET metadata_location = 'b' WHERE metadata_location = 'a'");

    assert_eq!(first, 1, "the update that matched");
    assert_eq!(second, 0, "the same update, now losing the race");
}

/// The regression guard for `OPFLAG_NCHANGE`'s existence. Both `UPDATE`
/// plans change exactly one row and must both say so, even though they
/// emit different numbers of `Insert`/`Delete` opcodes to do it.
///
/// Plan selection is #675's rule: the two-pass plan is taken when the
/// `SET` clause touches the index the range predicate scans, the
/// single-pass plan when it does not.
#[test]
fn update_of_one_row_reports_one_under_both_plans() {
    // Two-pass: `SET n` touches the scanned index `t_n`.
    let mut two_pass = Db::new();
    two_pass.exec_ddl("CREATE TABLE t(n INTEGER, v TEXT)");
    two_pass.exec_ddl("CREATE INDEX t_n ON t(n)");
    for i in 1..=5 {
        two_pass.changes(&format!("INSERT INTO t VALUES ({i}, 'v{i}')"));
    }
    let counted = two_pass.changes("UPDATE t SET n = n + 100 WHERE n > 4");

    // Single-pass: `SET v` leaves the scanned index alone.
    let mut single_pass = Db::new();
    single_pass.exec_ddl("CREATE TABLE t(n INTEGER, v TEXT)");
    single_pass.exec_ddl("CREATE INDEX t_n ON t(n)");
    for i in 1..=5 {
        single_pass.changes(&format!("INSERT INTO t VALUES ({i}, 'v{i}')"));
    }
    let counted_single = single_pass.changes("UPDATE t SET v = 'x' WHERE n > 4");

    assert_eq!(counted, 1, "two-pass plan counted its own scratch writes");
    assert_eq!(counted_single, 1, "single-pass plan");
}

/// `Some(0)` and `None` are different answers: an `UPDATE` that matched
/// nothing is a counting statement that counted zero, which is what a
/// lost optimistic-concurrency race looks like.
#[test]
fn update_matching_nothing_reports_some_zero_not_none() {
    let mut db = Db::new();
    db.exec_ddl("CREATE TABLE t(n INTEGER)");
    db.changes("INSERT INTO t VALUES (1)");

    let outcome = db.step("UPDATE t SET n = 2 WHERE n = 999");

    assert_eq!(
        outcome.changes,
        Some(0),
        "an UPDATE whose WHERE matches nothing still has a count"
    );
}

/// A `SELECT` has no rows-changed count, so a connection tracking
/// `sqlite3_changes()` leaves its stored value alone rather than zeroing
/// it (spec 013/Req 1's second scenario).
///
/// Asserted against `Program::counts_changes` rather than through
/// `execute_transaction_step_counted`, because `compile_statement`
/// handles write and DDL statements only — a `SELECT` reaches the engine
/// by a different route entirely (`compile_select*` + `execute_with_db`),
/// which has no count to clobber in the first place. The static
/// discriminator is the thing a future facade will consult, so it is the
/// thing worth pinning.
#[test]
fn select_is_not_a_counting_statement() {
    let mut db = Db::new();
    db.exec_ddl("CREATE TABLE t(n INTEGER)");
    db.changes("INSERT INTO t VALUES (1)");

    for sql in ["SELECT n FROM t WHERE n = 999", "SELECT count(*) FROM t"] {
        let program = db.compile_select(sql);
        assert!(
            !program.counts_changes(),
            "{sql} claimed a rows-changed count"
        );
    }

    // And a statement that does have one still says so, so the assertion
    // above is not passing for want of any flagged program at all.
    let insert = db.compile("INSERT INTO t VALUES (2)");
    assert!(insert.counts_changes());
}

#[test]
fn insert_and_delete_count_their_rows() {
    let mut db = Db::new();
    db.exec_ddl("CREATE TABLE t(n INTEGER)");
    for i in 0..7 {
        assert_eq!(db.changes(&format!("INSERT INTO t VALUES ({i})")), 1);
    }
    assert_eq!(db.count_rows("SELECT count(*) FROM t"), 7);

    assert_eq!(db.changes("DELETE FROM t WHERE n < 3"), 3, "partial delete");
    assert_eq!(db.changes("DELETE FROM t"), 4, "the rest");
    assert_eq!(db.changes("DELETE FROM t"), 0, "already empty");
}

/// Index maintenance is a row-adjacent write, not a row change: the same
/// statements against a table with three indexes must report the same
/// numbers as against a table with none.
#[test]
fn index_maintenance_does_not_count() {
    let counts = |indexed: bool| {
        let mut db = Db::new();
        db.exec_ddl("CREATE TABLE t(a INTEGER, b TEXT, c TEXT)");
        if indexed {
            db.exec_ddl("CREATE INDEX t_a ON t(a)");
            db.exec_ddl("CREATE INDEX t_b ON t(b)");
            db.exec_ddl("CREATE UNIQUE INDEX t_c ON t(c)");
        }
        let mut seen = Vec::new();
        for i in 0..4 {
            seen.push(db.changes(&format!("INSERT INTO t VALUES ({i}, 'b{i}', 'c{i}')")));
        }
        seen.push(db.changes("UPDATE t SET b = 'z' WHERE a >= 2"));
        seen.push(db.changes("DELETE FROM t WHERE a < 2"));
        seen
    };

    assert_eq!(counts(true), counts(false));
    assert_eq!(counts(false), vec![1, 1, 1, 1, 2, 2]);
}

/// DDL is not a counting statement either, even though `CREATE TABLE`
/// writes a `sqlite_master` row.
#[test]
fn ddl_has_no_count() {
    let mut db = Db::new();
    assert_eq!(db.step("CREATE TABLE t(n INTEGER)").changes, None);
    assert_eq!(db.step("CREATE INDEX t_n ON t(n)").changes, None);
}
