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
use sqlite_rs::parser::split_statements;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::execute_transaction_step;
use sqlite_rs::vfs::UnixVfs;

use crate::common::fatal;

pub fn run_exec(path: &Path, sql: &str) -> ExitCode {
    let (header, pager) = match dump::open(&UnixVfs, path) {
        Ok(v) => v,
        Err(e) => return fatal(path, &e),
    };
    let pager = Rc::new(RefCell::new(pager));

    let mut autocommit = true;
    for stmt in split_statements(sql) {
        // Re-read the schema before every statement, not just once up
        // front: a script that `CREATE TABLE`s and then writes to that
        // same table in a later statement needs the catalog to reflect
        // what already ran earlier in this same script.
        let schemas = {
            let borrowed = pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
            match read_schema(&mut schema_cursor, header.text_encoding) {
                Ok(s) => s,
                Err(e) => return fatal(path, &e),
            }
        };

        let program = match compile_statement(&stmt, &schemas) {
            Ok(p) => p,
            Err(e) => return fatal(path, &e),
        };

        match execute_transaction_step(&program, Rc::clone(&pager), header, autocommit) {
            Ok((_, ac)) => autocommit = ac,
            Err(e) => return fatal(path, &e),
        }
    }

    ExitCode::SUCCESS
}
