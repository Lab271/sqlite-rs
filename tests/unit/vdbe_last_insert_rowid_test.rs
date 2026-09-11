// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Last-inserted-rowid signal (spec 013 Requirement 1).
//!
//! `sqlite3_last_insert_rowid()` is the other half of what an embedding
//! consumer cannot work around: a row inserted into a table with a
//! surrogate key is unaddressable until the caller learns its rowid.
//!
//! Like the rows-changed counter, this is driven by a `P5` flag
//! (`OPFLAG_LASTROWID`) rather than by the opcode, and for a sharper reason.
//! An `UPDATE` rewrites a row as `Delete` + `Insert`, so an implementation
//! that hooked the `Insert` *opcode* would report the updated row's rowid
//! and quietly destroy the value a caller was about to use. Stock SQLite
//! avoids this by setting the flag only on the insert path
//! (`insert.c:2834`), and nesting the update inside the `OPFLAG_NCHANGE`
//! arm (`vdbe.c:5800`-`5803`, pinned 3.53.4).
//!
//! These tests pin the mechanism. `tests/corpus/last_insert_rowid_oracle_test.rs`
//! pins the answers against the pinned `sqlite3`.

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
use sqlite_rs::codegen::{compile_select_program, compile_statement, SelectOutcome};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::error::ParseOutcome;
use sqlite_rs::parser::parse_select;
use sqlite_rs::planner::Stats;
use sqlite_rs::schema::{read_schema, read_views, TableSchema, ViewSchema};
use sqlite_rs::vdbe::{
    execute_transaction_step_counted, Opcode, Program, StepOutcome, OPFLAG_LASTROWID,
};
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
    /// The connection-scoped value, folded the way spec 013's
    /// `Connection` will: `None` retains, `Some` replaces.
    retained: i64,
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
            retained: 0,
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

    fn compile(&self, sql: &str) -> Program {
        let (schemas, views) = self.catalog();
        if !sql.trim_start().to_ascii_uppercase().starts_with("SELECT") {
            return compile_statement(sql, &schemas, &views)
                .unwrap_or_else(|e| panic!("{sql} did not compile: {e}"));
        }
        let select = match parse_select(sql) {
            ParseOutcome::Accepted(select) => *select,
            ParseOutcome::Unsupported { message, .. } | ParseOutcome::Invalid { message, .. } => {
                panic!("{sql} did not parse: {message}")
            }
        };
        let stats: HashMap<String, Stats> = HashMap::new();
        match compile_select_program(&select, false, &schemas, &views, &stats) {
            Ok(SelectOutcome::Program(p)) => p,
            Ok(SelectOutcome::Eqp(_)) => panic!("{sql} compiled to EXPLAIN QUERY PLAN"),
            Err(e) => panic!("{sql} did not compile: {e}"),
        }
    }

    fn step(&mut self, sql: &str) -> StepOutcome {
        let program = self.compile(sql);
        let outcome = execute_transaction_step_counted(
            &program,
            Rc::clone(&self.pager),
            self.header,
            self.autocommit,
        )
        .unwrap_or_else(|e| panic!("{sql} failed: {e}"));
        self.autocommit = outcome.autocommit;
        self.retained = outcome.last_insert_rowid.unwrap_or(self.retained);
        outcome
    }

    /// The rowid this statement reported, if any.
    fn reported(&mut self, sql: &str) -> Option<i64> {
        self.step(sql).last_insert_rowid
    }
}

#[test]
fn an_insert_reports_the_rowid_it_used() {
    let mut db = Db::new();
    db.step("CREATE TABLE t(a INTEGER, b TEXT)");

    assert_eq!(db.reported("INSERT INTO t VALUES (1, 'x')"), Some(1));
    assert_eq!(db.reported("INSERT INTO t VALUES (2, 'y')"), Some(2));
    assert_eq!(db.reported("INSERT INTO t VALUES (3, 'z')"), Some(3));
}

/// The case that makes hooking `Insert` rather than `NewRowid` load-bearing.
///
/// When the caller supplies a value for an `INTEGER PRIMARY KEY`, that value
/// *is* the rowid and `NewRowid` never executes. An implementation that read
/// the rowid off `NewRowid`'s output register would report a stale value here
/// — and this is the shape a consumer with its own surrogate keys uses.
#[test]
fn an_explicit_integer_primary_key_reports_the_supplied_value() {
    let mut db = Db::new();
    db.step("CREATE TABLE u(id INTEGER PRIMARY KEY, v TEXT)");

    assert_eq!(
        db.reported("INSERT INTO u(id, v) VALUES (42, 'a')"),
        Some(42)
    );
    // Deliberately *lower* than the previous rowid: a stale-value bug
    // cannot hide behind a monotonically increasing sequence.
    assert_eq!(db.reported("INSERT INTO u(id, v) VALUES (7, 'b')"), Some(7));
}

/// An `UPDATE` emits an `Insert`, so this is the test that separates
/// "flagged by codegen" from "hooked on the opcode".
#[test]
fn an_update_reports_nothing_and_leaves_the_value_standing() {
    let mut db = Db::new();
    db.step("CREATE TABLE t(a INTEGER, b TEXT)");
    db.step("INSERT INTO t VALUES (1, 'x')");
    db.step("INSERT INTO t VALUES (2, 'y')");
    assert_eq!(db.retained, 2);

    // Matches row 1 — an opcode-level hook would report 1 here.
    assert_eq!(db.reported("UPDATE t SET b = 'z' WHERE a = 1"), None);
    assert_eq!(db.retained, 2, "an UPDATE moved the last-insert rowid");

    // And the two-pass plan (#675), which touches a scanned index.
    db.step("CREATE INDEX t_a ON t(a)");
    assert_eq!(db.reported("UPDATE t SET a = a + 10 WHERE a > 0"), None);
    assert_eq!(db.retained, 2, "the two-pass UPDATE plan moved the value");
}

#[test]
fn deletes_and_reads_report_nothing() {
    let mut db = Db::new();
    db.step("CREATE TABLE t(a INTEGER, b TEXT)");
    db.step("INSERT INTO t VALUES (1, 'x')");
    db.step("INSERT INTO t VALUES (2, 'y')");

    assert_eq!(db.reported("DELETE FROM t WHERE a = 1"), None);
    assert_eq!(db.reported("SELECT a FROM t"), None);
    assert_eq!(db.reported("DELETE FROM t"), None);
    assert_eq!(
        db.retained, 2,
        "a delete or a read moved the last-insert rowid"
    );
}

/// `None` and `Some(0)` are different answers, and so are `None` and
/// `Some(previous)`. A connection needs `None` to mean "leave the stored
/// value alone"; reporting the previous value instead would satisfy any
/// retention test while making "nothing inserted" indistinguishable from
/// "inserted the same rowid again".
#[test]
fn nothing_inserted_is_none_rather_than_the_previous_value() {
    let mut db = Db::new();
    db.step("CREATE TABLE t(a INTEGER)");
    db.step("INSERT INTO t VALUES (1)");
    assert_eq!(db.retained, 1);

    let reported = db.reported("UPDATE t SET a = 9 WHERE a = 1");
    assert_eq!(reported, None);
    assert_ne!(
        reported,
        Some(1),
        "a non-inserting statement echoed the retained value instead of None"
    );
}

/// Index maintenance writes index entries, not rows. Those go through
/// `IdxInsert`, but a table with indexes still emits exactly one flagged
/// `Insert` per row — so an indexed table must report the same rowid an
/// unindexed one does.
#[test]
fn index_maintenance_is_not_an_insert() {
    let mut db = Db::new();
    db.step("CREATE TABLE t(a INTEGER, b TEXT, c TEXT)");
    db.step("CREATE INDEX t_a ON t(a)");
    db.step("CREATE UNIQUE INDEX t_c ON t(c)");

    assert_eq!(db.reported("INSERT INTO t VALUES (1, 'b1', 'c1')"), Some(1));
    assert_eq!(db.reported("INSERT INTO t VALUES (2, 'b2', 'c2')"), Some(2));
}

/// Codegen's decision, asserted on the emitted program rather than only
/// through behaviour: exactly one instruction in an `INSERT` carries the
/// flag, and an `UPDATE` carries none despite emitting an `Insert`.
///
/// This is the structural counterpart to the behavioural tests above. If a
/// future change flags the `Insert` an `UPDATE` emits, this fails at the
/// point of the mistake instead of as a surprising rowid three layers away.
#[test]
fn only_an_inserts_table_write_carries_the_flag() {
    let mut db = Db::new();
    db.step("CREATE TABLE t(a INTEGER, b TEXT)");
    db.step("CREATE INDEX t_a ON t(a)");

    let flagged = |program: &Program| -> usize {
        program
            .instructions
            .iter()
            .filter(|i| i.p5 & OPFLAG_LASTROWID != 0)
            .count()
    };

    let insert = db.compile("INSERT INTO t VALUES (1, 'x')");
    assert_eq!(
        flagged(&insert),
        1,
        "an INSERT should flag exactly one instruction"
    );
    assert!(
        insert
            .instructions
            .iter()
            .any(|i| i.p5 & OPFLAG_LASTROWID != 0 && matches!(i.opcode, Opcode::Insert)),
        "the flagged instruction should be the table Insert"
    );

    let update = db.compile("UPDATE t SET b = 'z' WHERE a = 1");
    assert!(
        update
            .instructions
            .iter()
            .any(|i| matches!(i.opcode, Opcode::Insert)),
        "an UPDATE is expected to emit an Insert — otherwise this test proves nothing"
    );
    assert_eq!(
        flagged(&update),
        0,
        "an UPDATE must not flag any instruction with OPFLAG_LASTROWID"
    );

    let delete = db.compile("DELETE FROM t WHERE a = 1");
    assert_eq!(
        flagged(&delete),
        0,
        "a DELETE must not flag any instruction"
    );
}

/// `OPFLAG_LASTROWID` nests inside `OPFLAG_NCHANGE` upstream
/// (`vdbe.c:5800` asserts the implication). Reproduce that as a property of
/// every program this compiler emits: no instruction may carry the rowid
/// flag without also carrying the change flag.
#[test]
fn the_rowid_flag_never_appears_without_the_change_flag() {
    let mut db = Db::new();
    db.step("CREATE TABLE t(a INTEGER, b TEXT)");
    db.step("CREATE INDEX t_a ON t(a)");
    db.step("CREATE TABLE u(id INTEGER PRIMARY KEY, v TEXT)");

    for sql in [
        "INSERT INTO t VALUES (1, 'x')",
        "INSERT INTO u(id, v) VALUES (5, 'y')",
        "UPDATE t SET b = 'z' WHERE a = 1",
        "UPDATE t SET a = a + 1 WHERE a > 0",
        "DELETE FROM t WHERE a = 1",
        "DELETE FROM t",
    ] {
        let program = db.compile(sql);
        for (pc, i) in program.instructions.iter().enumerate() {
            if i.p5 & OPFLAG_LASTROWID != 0 {
                assert!(
                    i.p5 & sqlite_rs::vdbe::OPFLAG_NCHANGE != 0,
                    "{sql}: instruction {pc} ({:?}) carries OPFLAG_LASTROWID without \
                     OPFLAG_NCHANGE, which stock SQLite asserts cannot happen",
                    i.opcode
                );
            }
        }
    }
}
