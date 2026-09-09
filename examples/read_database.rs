// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Open an existing database file, list its tables, and read every row of
//! one of them.
//!
//! Run with: `cargo run --example read_database`

use std::error::Error;
use std::path::Path;

use sqlite_rs::api::{Connection, OpenMode};

fn main() -> Result<(), Box<dyn Error>> {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/fixtures/sample.db");

    // `ReadWrite` rather than the default `ReadWriteCreate`, so a typo in
    // the path is an error instead of a new empty database. `ReadOnly`
    // would also refuse every write for the connection's whole life.
    let conn = Connection::open_with(&fixture, OpenMode::ReadOnly)?;
    println!("opened {} read-only\n", fixture.display());

    let tables = conn.table_names()?;
    println!("{} table(s): {}\n", tables.len(), tables.join(", "));

    for table in &tables {
        // Table names cannot be bound as parameters — a placeholder is a
        // *value*, not an identifier — so this interpolates a name that
        // came from the catalog itself, not from user input.
        let count: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"))?
            .ok_or("count() returned no row")?
            .get(0)?;
        println!("{table}: {count} row(s)");
    }

    let Some(first) = tables.first() else {
        println!("\nno tables to read");
        return Ok(());
    };

    println!("\nevery row of {first}:");
    let mut rows = conn.query(&format!("SELECT * FROM {first}"))?;
    println!("  columns: {:?}", rows.column_names().join(", "));
    while let Some(row) = rows.next_row()? {
        // `value()` hands back the raw storage class, for a reader that
        // does not know the column types ahead of time.
        let cells: Vec<String> = (0..row.len())
            .map(|i| match row.value(i) {
                Some(v) => format!("{v:?}"),
                None => "<missing>".to_string(),
            })
            .collect();
        println!("  {}", cells.join(" | "));
    }

    // Read-only means read-only: this is refused rather than attempted.
    match conn.execute(&format!("DELETE FROM {first}")) {
        Err(e) => println!("\nattempted write -> {e}"),
        Ok(_) => println!("\nunexpectedly wrote to a read-only connection"),
    }

    Ok(())
}
