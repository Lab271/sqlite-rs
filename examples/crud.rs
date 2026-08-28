// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! A full create-read-update-delete cycle, including an explicit
//! `BEGIN`/`COMMIT` transaction.
//!
//! This crate has no API to create a brand-new database file from
//! nothing (only to open an already-valid one), so this example copies
//! a checked-in empty fixture to a scratch path first, then builds a
//! table on top of it.
//!
//! Run with: `cargo run --example crud`

use std::cell::RefCell;
use std::error::Error;
use std::path::Path;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{
    compile_select_with_catalog, compile_statement, resolve_from_table_schema,
};
use sqlite_rs::dump;
use sqlite_rs::format::format_query_value;
use sqlite_rs::parser::{parse_select, split_statements, ParseOutcome};
use sqlite_rs::schema::{read_schema, read_views};
use sqlite_rs::vdbe::{execute_transaction_step, execute_with_db};
use sqlite_rs::vfs::{PageSource, UnixVfs};

fn main() -> Result<(), Box<dyn Error>> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/fixtures/empty.db");
    let scratch_dir =
        std::env::temp_dir().join(format!("sqlite-rs-crud-example-{}", std::process::id()));
    std::fs::create_dir_all(&scratch_dir)?;
    let scratch_db = scratch_dir.join("crud.db");
    std::fs::copy(&fixture, &scratch_db)?;

    let (header, pager) = dump::open(&UnixVfs, &scratch_db)?;
    let pager = Rc::new(RefCell::new(pager));

    let script = "
        CREATE TABLE todos(id INTEGER PRIMARY KEY, task TEXT, done INTEGER);
        BEGIN;
        INSERT INTO todos(id, task, done) VALUES (1, 'write examples', 0);
        INSERT INTO todos(id, task, done) VALUES (2, 'ship it', 0);
        UPDATE todos SET done = 1 WHERE id = 1;
        DELETE FROM todos WHERE id = 2;
        COMMIT;
    ";

    let mut autocommit = true;
    for stmt in split_statements(script) {
        let (schemas, views) = {
            let borrowed = pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
            let schemas = read_schema(&mut schema_cursor, header.text_encoding)?;
            let mut view_cursor = TableCursor::new(&*borrowed, &header, 1);
            let views = read_views(&mut view_cursor, header.text_encoding)?;
            (schemas, views)
        };

        let program = compile_statement(&stmt, &schemas, &views).map_err(|e| e.to_string())?;
        let (_, ac) = execute_transaction_step(&program, Rc::clone(&pager), header, autocommit)
            .map_err(|e| e.to_string())?;
        autocommit = ac;
    }

    // Read back the final state through the same shared pager.
    let schemas = {
        let borrowed = pager.borrow();
        let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
        read_schema(&mut schema_cursor, header.text_encoding)?
    };
    let select = match parse_select("SELECT id, task, done FROM todos") {
        ParseOutcome::Accepted(select) => *select,
        _ => return Err("failed to parse the readback query".into()),
    };
    let from = select.from.as_ref().ok_or("SELECT has no FROM clause")?;
    let table = resolve_from_table_schema(&from.first, &schemas).map_err(|e| e.to_string())?;
    let select_program =
        compile_select_with_catalog(&select, &table, &schemas).map_err(|e| e.to_string())?;
    let source: Rc<dyn PageSource> = pager;
    let rows = execute_with_db(&select_program, source, header).map_err(|e| e.to_string())?;

    println!("Final todos:");
    for row in rows {
        let rendered: Vec<String> = row
            .iter()
            .map(|v| String::from_utf8_lossy(&format_query_value(v)).into_owned())
            .collect();
        println!("  {}", rendered.join(" | "));
    }

    std::fs::remove_dir_all(&scratch_dir).ok();
    Ok(())
}
