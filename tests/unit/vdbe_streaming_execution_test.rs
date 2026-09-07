// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Acceptance for the streaming execution primitive (#683, ADR-0038):
//! `Execution::next_row` must deliver exactly the rows
//! `execute_transaction_step` delivers, in exactly the same order,
//! without buffering them all first.
//!
//! `run()` is implemented as a wrapper over `next_row`, so equivalence
//! is structural for anything the batch path can reach. What these tests
//! guard is the part that is *not* structural: the `pending` FIFO, whose
//! necessity depends on how many rows a single dispatch can emit, and
//! the terminal-state handling a streaming caller can observe but a
//! batch caller cannot.

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
use sqlite_rs::codegen::{
    compile_select_with_catalog, compile_statement, resolve_from_table_schema,
};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Value;
use sqlite_rs::schema::{read_schema, read_views};
use sqlite_rs::vdbe::{execute_transaction_step, Execution, Vm};
use sqlite_rs::vfs::{MemoryVfs, PageSource, Vfs};

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
    vfs: MemoryVfs,
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
            vfs,
            pager: Rc::new(RefCell::new(pager)),
            header,
            autocommit: true,
        }
    }

    /// Replaces `index`'s root page with a structurally valid but *empty*
    /// index leaf, so `PRAGMA integrity_check` reports an entry-count
    /// mismatch for it.
    ///
    /// This is the only way to reach the multi-row-per-dispatch path:
    /// `pragma::integrity_check` is the sole `Vm::emit_row` caller that
    /// emits more than one row from a single dispatch, and a clean
    /// database reports exactly one row (`"ok"`). Two details were
    /// measured rather than assumed: zeroing the page is no good (a
    /// malformed page errors instead of producing problem rows), and
    /// emptying *one* index yields only one problem row — the count
    /// mismatch — so several indexes have to be emptied to get several
    /// rows out of the single dispatch.
    fn empty_out_index(&mut self, index: &str) {
        let page_size = 4096u32;
        let root = {
            let borrowed = self.pager.borrow();
            let mut cursor = TableCursor::new(&*borrowed, &self.header, 1);
            let schemas = read_schema(&mut cursor, self.header.text_encoding).unwrap();
            schemas
                .iter()
                .flat_map(|t| t.indexes.iter())
                .find(|i| i.name == index)
                .expect("index must exist")
                .root_page
        };

        let mut page = vec![0u8; page_size as usize];
        page[0] = 0x0A; // leaf index b-tree page
        page[1..3].copy_from_slice(&0u16.to_be_bytes()); // no freeblocks
        page[3..5].copy_from_slice(&0u16.to_be_bytes()); // zero cells
        page[5..7].copy_from_slice(&u16::try_from(page_size).unwrap().to_be_bytes());
        page[7] = 0; // no fragmented bytes

        // Page numbers are 1-based; `saturating_sub`/`checked_mul` keep
        // the crate's `arithmetic_side_effects` lint satisfied without
        // pretending a root page of 0 is reachable here.
        let offset = u64::from(root.saturating_sub(1))
            .checked_mul(u64::from(page_size))
            .expect("page offset must fit in u64");
        let file = self.vfs.open_write(Path::new("/test.db")).unwrap();
        file.write_at(&page, offset).unwrap();
        file.sync().unwrap();

        // The live pager has the old page cached; a fresh one is the
        // simplest way to read the corrupted file.
        let pager = Pager::open(&self.vfs, Path::new("/test.db"), page_size).unwrap();
        self.pager = Rc::new(RefCell::new(pager));
        self.autocommit = true;
    }

    /// `compile_statement` deliberately does not handle `SELECT` —
    /// query compilation needs the resolved `FROM` table, and the
    /// richer join/compound/stats dispatch lives in the CLI. Single-table
    /// `SELECT`s are compiled the way `examples/query.rs` does it; every
    /// other statement goes through the ordinary dispatcher.
    fn compile(&self, sql: &str) -> sqlite_rs::vdbe::Program {
        let (schemas, views) = {
            let borrowed = self.pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &self.header, 1);
            let schemas = read_schema(&mut schema_cursor, self.header.text_encoding).unwrap();
            let mut view_cursor = TableCursor::new(&*borrowed, &self.header, 1);
            let views = read_views(&mut view_cursor, self.header.text_encoding).unwrap();
            (schemas, views)
        };
        if sql.trim_start().to_ascii_uppercase().starts_with("SELECT") {
            let select = match parse_select(sql) {
                ParseOutcome::Accepted(select) => *select,
                other => panic!("failed to parse {sql}: {other:?}"),
            };
            let from = select.from.as_ref().expect("test SELECTs all have a FROM");
            let table = resolve_from_table_schema(&from.first, &schemas).unwrap();
            return compile_select_with_catalog(&select, &table, &schemas).unwrap();
        }
        compile_statement(sql, &schemas, &views).unwrap()
    }

    /// The batch path, as any existing caller uses it.
    fn exec(&mut self, sql: &str) -> Vec<Vec<Value>> {
        let program = self.compile(sql);
        let (rows, autocommit) = execute_transaction_step(
            &program,
            Rc::clone(&self.pager),
            self.header,
            self.autocommit,
        )
        .unwrap();
        self.autocommit = autocommit;
        rows
    }

    /// The streaming path, pulled one row at a time.
    ///
    /// Read-only (`Vm::with_db`): a streaming caller cannot thread the
    /// autocommit flag from outside the crate, and does not need to —
    /// #683 scopes streaming to result-producing statements. Write
    /// equivalence is structural, since `run` is a wrapper over
    /// `next_row`, and is covered by the existing suite.
    fn stream(&self, sql: &str) -> Vec<Vec<Value>> {
        let program = self.compile(sql);
        let source: Rc<dyn PageSource> = Rc::clone(&self.pager) as Rc<dyn PageSource>;
        let mut execution = Execution::new(Vm::with_db(source, self.header), &program);
        let mut rows = Vec::new();
        while let Some(row) = execution.next_row().unwrap() {
            rows.push(row);
        }
        rows
    }

    fn seed(&mut self, rows: usize) {
        self.exec("CREATE TABLE t(a INTEGER, b TEXT)");
        for i in 0..rows {
            self.exec(&format!("INSERT INTO t VALUES ({i}, 'row{i}')"));
        }
    }
}

fn text_rows(rows: &[Vec<Value>]) -> Vec<String> {
    rows.iter()
        .map(|r| match &r[0] {
            Value::Text(s) => s.to_string(),
            other => panic!("expected TEXT row, got {other:?}"),
        })
        .collect()
}

/// The core claim: same rows, same order, for a range of result shapes.
#[test]
fn streaming_matches_batch_for_every_result_shape() {
    let mut db = Db::new();
    db.seed(25);

    for sql in [
        "SELECT a, b FROM t",
        "SELECT a FROM t WHERE a > 20",
        "SELECT a FROM t LIMIT 3",
        "SELECT count(*) FROM t",
        "SELECT a FROM t ORDER BY a DESC",
        "SELECT a FROM t WHERE a > 9999", // empty result
    ] {
        let batch = db.exec(sql);
        let streamed = db.stream(sql);
        assert_eq!(batch, streamed, "streaming diverged from batch for: {sql}");
    }
}

/// The regression guard the `pending` FIFO exists for.
///
/// `Vm::emit_row` has three callers, and `pragma::integrity_check` emits
/// one row *per problem found* from a single dispatch. A drain that
/// assumed one row per step — popping off the back of `Vm`'s row vector
/// rather than through a queue — would reverse this output silently,
/// because every other opcode emits at most one row and looks correct
/// either way.
///
/// A clean database reports a single `"ok"` row, which would pass any
/// ordering regardless, so this deliberately corrupts an index first to
/// force a genuinely multi-row result. No other test in the suite
/// reaches this path.
#[test]
fn streaming_preserves_multi_row_single_dispatch_order() {
    let mut db = Db::new();
    db.exec("CREATE TABLE t(a INTEGER, b TEXT)");
    db.exec("CREATE INDEX t_a ON t(a)");
    db.exec("CREATE INDEX t_b ON t(b)");
    db.exec("CREATE INDEX t_ab ON t(a, b)");
    for i in 0..12 {
        db.exec(&format!("INSERT INTO t VALUES ({i}, 'row{i}')"));
    }
    assert_eq!(
        text_rows(&db.exec("PRAGMA integrity_check")),
        vec!["ok"],
        "database should start clean"
    );

    for index in ["t_a", "t_b", "t_ab"] {
        db.empty_out_index(index);
    }

    let batch = text_rows(&db.exec("PRAGMA integrity_check"));
    let streamed = text_rows(&db.stream("PRAGMA integrity_check"));

    // The point of the whole test: more than one row from one dispatch.
    assert!(
        batch.len() > 1,
        "corruption should yield several problem rows, got {batch:?}"
    );
    assert_eq!(batch.len(), 3, "expected one problem per emptied index");
    assert_eq!(
        batch, streamed,
        "streaming reordered a multi-row single-dispatch result"
    );
}

/// A wide `SELECT` over many rows: the ordinary one-row-per-dispatch
/// path, asserted positionally rather than just by length, so a
/// reversal or an off-by-one would fail rather than pass on count.
#[test]
fn streaming_yields_rows_in_emission_order() {
    let mut db = Db::new();
    db.seed(50);

    let streamed = db.stream("SELECT a FROM t");
    assert_eq!(streamed.len(), 50);
    for (i, row) in streamed.iter().enumerate() {
        assert_eq!(
            row[0],
            Value::Integer(i as i64),
            "row {i} out of order or wrong"
        );
    }
}

/// Halting is terminal. A batch caller can never observe this, because
/// it never holds an `Execution` after `run` returns; a streaming caller
/// can, and must not be able to re-enter a finished program.
#[test]
fn polling_past_the_end_keeps_returning_none() {
    let mut db = Db::new();
    db.seed(3);
    let program = db.compile("SELECT a FROM t");
    let source: Rc<dyn PageSource> = Rc::clone(&db.pager) as Rc<dyn PageSource>;
    let mut execution = Execution::new(Vm::with_db(source, db.header), &program);

    let mut seen = 0;
    while execution.next_row().unwrap().is_some() {
        seen += 1;
    }
    assert_eq!(seen, 3);

    for _ in 0..3 {
        assert!(
            execution.next_row().unwrap().is_none(),
            "a halted program must stay halted"
        );
    }
}

/// Abandoning a stream part-way must not disturb the database or the
/// next statement — the property a `Statement` handle dropped mid-result
/// depends on.
#[test]
fn abandoning_a_stream_leaves_the_database_usable() {
    let mut db = Db::new();
    db.seed(20);

    {
        let program = db.compile("SELECT a FROM t");
        let source: Rc<dyn PageSource> = Rc::clone(&db.pager) as Rc<dyn PageSource>;
        let mut execution = Execution::new(Vm::with_db(source, db.header), &program);
        assert!(execution.next_row().unwrap().is_some());
        // dropped here with 19 rows unread
    }

    assert_eq!(db.exec("SELECT count(*) FROM t")[0][0], Value::Integer(20));
    db.exec("INSERT INTO t VALUES (999, 'after')");
    assert_eq!(db.exec("SELECT count(*) FROM t")[0][0], Value::Integer(21));
}

/// `autocommit` is what `execute_transaction_step` threads between
/// statements, so the streaming primitive has to report it truthfully
/// for a future facade to chain statements on one pager.
#[test]
fn autocommit_is_reported_after_a_read_only_statement() {
    let mut db = Db::new();
    db.seed(2);
    let program = db.compile("SELECT a FROM t");
    let source: Rc<dyn PageSource> = Rc::clone(&db.pager) as Rc<dyn PageSource>;
    let mut execution = Execution::new(Vm::with_db(source, db.header), &program);
    while execution.next_row().unwrap().is_some() {}
    assert!(
        execution.autocommit(),
        "a plain SELECT must leave autocommit on"
    );
}
