//! Opens an existing SQLite file, lists its tables, and iterates every
//! row of one table.
//!
//! Run with: `cargo run --example read_database`

use std::error::Error;
use std::path::Path;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_select_with_catalog, resolve_from_table_schema};
use sqlite_rs::dump;
use sqlite_rs::format::format_query_value;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::execute_with_db;
use sqlite_rs::vfs::{PageSource, UnixVfs};

fn main() -> Result<(), Box<dyn Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/fixtures/sample.db");

    // Opening a database parses its header and returns a `Pager` over it.
    let (header, pager) = dump::open(&UnixVfs, &path)?;

    // Schema introspection: `sqlite_schema` always lives at root page 1.
    let mut schema_cursor = TableCursor::new(&pager, &header, 1);
    let schemas = read_schema(&mut schema_cursor, header.text_encoding)?;

    println!("Tables:");
    for schema in &schemas {
        println!("  {} ({} columns)", schema.name, schema.columns.len());
    }

    // Row iteration: resolve the table, compile `SELECT * FROM users`,
    // then execute it against the same `Pager` (as a read-only `PageSource`).
    let select = match parse_select("SELECT * FROM users") {
        ParseOutcome::Accepted(select) => *select,
        _ => return Err("failed to parse SELECT * FROM users".into()),
    };
    let from = select.from.as_ref().ok_or("SELECT has no FROM clause")?;
    let table = resolve_from_table_schema(&from.first, &schemas).map_err(|e| e.to_string())?;
    let program =
        compile_select_with_catalog(&select, &table, &schemas).map_err(|e| e.to_string())?;

    let source: Rc<dyn PageSource> = Rc::new(pager);
    let rows = execute_with_db(&program, source, header).map_err(|e| e.to_string())?;

    println!("\nRows in users:");
    for row in rows {
        let rendered: Vec<String> = row
            .iter()
            .map(|v| String::from_utf8_lossy(&format_query_value(v)).into_owned())
            .collect();
        println!("  {}", rendered.join(" | "));
    }

    Ok(())
}
