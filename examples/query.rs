// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Runs a parameterized `SELECT` against a database, binding a `?1`
//! placeholder before executing.
//!
//! Run with: `cargo run --example query`

use std::error::Error;
use std::path::Path;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_select_with_catalog, resolve_from_table_schema};
use sqlite_rs::dump;
use sqlite_rs::format::format_query_value;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Value;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::execute_with_db_and_params;
use sqlite_rs::vfs::{PageSource, UnixVfs};

fn main() -> Result<(), Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/fixtures/sample.db");
    let (header, pager) = dump::open(&UnixVfs, &path)?;

    let mut schema_cursor = TableCursor::new(&pager, &header, 1);
    let schemas = read_schema(&mut schema_cursor, header.text_encoding)?;

    // Prepare: parse and compile once. `?1` is a placeholder bound at
    // execution time via `execute_with_db_and_params`.
    let select = match parse_select("SELECT name, age FROM users WHERE id = ?1") {
        ParseOutcome::Accepted(select) => *select,
        _ => return Err("failed to parse the query".into()),
    };
    let from = select.from.as_ref().ok_or("SELECT has no FROM clause")?;
    let table = resolve_from_table_schema(&from.first, &schemas).map_err(|e| e.to_string())?;
    let program =
        compile_select_with_catalog(&select, &table, &schemas).map_err(|e| e.to_string())?;

    let source: Rc<dyn PageSource> = Rc::new(pager);

    // Bind and run for a couple of different parameter values.
    for id in [1_i64, 3] {
        let rows = execute_with_db_and_params(
            &program,
            Rc::clone(&source),
            header,
            vec![Value::Integer(id)],
        )
        .map_err(|e| e.to_string())?;
        for row in rows {
            let rendered: Vec<String> = row
                .iter()
                .map(|v| String::from_utf8_lossy(&format_query_value(v)).into_owned())
                .collect();
            println!("id={id}: {}", rendered.join(" | "));
        }
    }

    Ok(())
}
