//! `exec <file> "<SQL>"`: runs one INSERT/UPDATE/DELETE/CREATE TABLE/DROP
//! TABLE/CREATE INDEX/DROP INDEX statement against a writable `Pager`
//! (#215's write-path CLI surface — Phase 4 of the V3 epic, #161).
//! Matches stock `sqlite3`'s CLI behavior of printing nothing on success
//! for a bare DML/DDL statement (no `.echo`/`-changes` flag requested).

use std::path::Path;
use std::process::ExitCode;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_statement;
use sqlite_rs::dump;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::execute_with_writable_db;
use sqlite_rs::vfs::UnixVfs;

use crate::common::fatal;

pub fn run_exec(path: &Path, sql: &str) -> ExitCode {
    let (header, pager) = match dump::open(&UnixVfs, path) {
        Ok(v) => v,
        Err(e) => return fatal(path, &e),
    };

    let schemas = {
        let mut schema_cursor = TableCursor::new(&pager, &header, 1);
        match read_schema(&mut schema_cursor, header.text_encoding) {
            Ok(s) => s,
            Err(e) => return fatal(path, &e),
        }
    };

    let program = match compile_statement(sql, &schemas) {
        Ok(p) => p,
        Err(e) => return fatal(path, &e),
    };

    match execute_with_writable_db(&program, pager, header) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => fatal(path, &e),
    }
}
