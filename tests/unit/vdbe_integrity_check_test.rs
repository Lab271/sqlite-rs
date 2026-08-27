// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! End-to-end acceptance for `PRAGMA integrity_check`/`quick_check`
//! (#540, #541): builds a small database purely through this crate's
//! own write path (`CREATE TABLE`/`CREATE INDEX`/`INSERT`), then runs
//! both pragmas through the full parse -> codegen -> VDBE pipeline and
//! checks the emitted rows.

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
use sqlite_rs::codegen::compile_statement;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::record::Value;
use sqlite_rs::schema::{read_schema, read_views};
use sqlite_rs::vdbe::execute_transaction_step;
use sqlite_rs::vfs::MemoryVfs;

fn empty_db(page_size: u32) -> (MemoryVfs, DatabaseHeader) {
    let mut page1 = vec![0u8; page_size as usize];
    page1[0..16].copy_from_slice(b"SQLite format 3\0");
    page1[16..18].copy_from_slice(&u16::try_from(page_size).unwrap().to_be_bytes());
    page1[18] = 1;
    page1[19] = 1;
    page1[28..32].copy_from_slice(&1u32.to_be_bytes());
    page1[56..60].copy_from_slice(&1u32.to_be_bytes());
    // Empty leaf table b-tree header at offset 100 (8-byte b-tree page
    // header: type=0x0D leaf-table, 0 freeblocks, 0 cells, cell-content
    // area starts at the top of the page, 0 fragmented bytes).
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

    fn exec(&mut self, sql: &str) -> Vec<Vec<Value>> {
        let (schemas, views) = {
            let borrowed = self.pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &self.header, 1);
            let schemas = read_schema(&mut schema_cursor, self.header.text_encoding).unwrap();
            let mut view_cursor = TableCursor::new(&*borrowed, &self.header, 1);
            let views = read_views(&mut view_cursor, self.header.text_encoding).unwrap();
            (schemas, views)
        };
        let program = compile_statement(sql, &schemas, &views).unwrap();
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
}

fn text_rows(rows: &[Vec<Value>]) -> Vec<String> {
    rows.iter()
        .map(|r| match &r[0] {
            Value::Text(s) => s.to_string(),
            other => panic!("expected TEXT row, got {other:?}"),
        })
        .collect()
}

#[test]
fn integrity_check_on_a_well_formed_database_reports_ok() {
    let mut db = Db::new();
    db.exec("CREATE TABLE t(a INTEGER, b TEXT)");
    db.exec("CREATE INDEX t_b ON t(b)");
    for i in 0..20 {
        db.exec(&format!("INSERT INTO t VALUES ({i}, 'row{i}')"));
    }

    assert_eq!(text_rows(&db.exec("PRAGMA integrity_check")), vec!["ok"]);
    assert_eq!(text_rows(&db.exec("PRAGMA quick_check")), vec!["ok"]);
}

#[test]
fn integrity_check_on_an_empty_database_reports_ok() {
    let mut db = Db::new();
    assert_eq!(text_rows(&db.exec("PRAGMA integrity_check")), vec!["ok"]);
}

#[test]
fn integrity_check_covers_multiple_tables_and_indexes() {
    let mut db = Db::new();
    db.exec("CREATE TABLE t1(a INTEGER)");
    db.exec("CREATE TABLE t2(b INTEGER, c INTEGER)");
    db.exec("CREATE INDEX t2_b ON t2(b)");
    db.exec("CREATE INDEX t2_c ON t2(c)");
    for i in 0..50 {
        db.exec(&format!("INSERT INTO t1 VALUES ({i})"));
        db.exec(&format!("INSERT INTO t2 VALUES ({i}, {})", i * 2));
    }

    assert_eq!(text_rows(&db.exec("PRAGMA integrity_check")), vec!["ok"]);
}
