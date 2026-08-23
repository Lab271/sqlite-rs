//! Test-only helper binary: a genuine second OS process that runs a
//! tight `BEGIN IMMEDIATE; INSERT; UPDATE; COMMIT;` loop against a
//! database, forever — #361's crash-torture test (`tests/corpus/
//! crash_torture_test.rs`) `kill -9`s it mid-loop from the outside and
//! checks recovery, the same way `lock_probe` gives the lock tests a
//! real second process to observe contention from (a same-process
//! child thread can't be `kill -9`'d independently of the test
//! process itself).
//!
//! Usage: `write_loop_probe <db path>` — the table `t(a INTEGER)` must
//! already exist; this never creates it, so a caller controls the
//! starting schema.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_statement;
use sqlite_rs::dump;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::execute_transaction_step;
use sqlite_rs::vfs::UnixVfs;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: write_loop_probe <db path>");
    let path = Path::new(&path);

    let (header, pager) = dump::open(&UnixVfs, path).expect("open db");
    let pager = Rc::new(RefCell::new(pager));
    let mut autocommit = true;
    let mut n: i64 = 0;

    loop {
        n = n.wrapping_add(1);
        let schemas = {
            let borrowed = pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
            read_schema(&mut schema_cursor, header.text_encoding).expect("read schema")
        };

        for stmt in [
            "BEGIN IMMEDIATE".to_string(),
            format!("INSERT INTO t VALUES ({n})"),
            format!("UPDATE t SET a = a + 1 WHERE a = {n}"),
            "COMMIT".to_string(),
        ] {
            let program = compile_statement(&stmt, &schemas, &[])
                .unwrap_or_else(|e| panic!("compiling {stmt:?}: {e}"));
            let (_, ac) = execute_transaction_step(&program, Rc::clone(&pager), header, autocommit)
                .unwrap_or_else(|e| panic!("running {stmt:?}: {e}"));
            autocommit = ac;
        }
    }
}
