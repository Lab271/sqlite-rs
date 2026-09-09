// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Prepare a `SELECT` once, then run it with different bound parameters.
//!
//! Run with: `cargo run --example query`

use std::error::Error;

use sqlite_rs::api::{Connection, Value};

fn main() -> Result<(), Box<dyn Error>> {
    // No file needed: an in-memory database exercises the same pager,
    // b-tree and journal code a file does.
    let conn = Connection::open_in_memory()?;

    conn.execute_batch(
        "CREATE TABLE fruit(id INTEGER, name TEXT, grams INTEGER);
         INSERT INTO fruit VALUES (1, 'apple', 150);
         INSERT INTO fruit VALUES (2, 'banana', 120);
         INSERT INTO fruit VALUES (3, 'cherry', 8);",
    )?;

    // Compiled once. `param_count` is the largest `?NNN` index the
    // statement uses, matching `sqlite3_bind_parameter_count`.
    let by_id = conn.prepare("SELECT name, grams FROM fruit WHERE id = ?1")?;
    println!(
        "prepared a statement wanting {} parameter(s)",
        by_id.param_count()
    );
    println!("columns: {:?}\n", by_id.column_names());

    for id in [1i64, 2, 3, 99] {
        match by_id.query_row(vec![Value::from(id)])? {
            Some(row) => {
                // Typed reads, by index or by name.
                let name: String = row.get(0)?;
                let grams: i64 = row.get_by_name("grams")?;
                println!("id {id}: {name} ({grams} g)");
            }
            None => println!("id {id}: no such row"),
        }
    }

    // Binding the wrong number of parameters is refused rather than
    // silently bound to NULL — which is the point of a statement handle.
    match by_id.execute(vec![]) {
        Err(e) => println!("\nno parameters bound -> {e}"),
        Ok(_) => println!("\nunexpectedly accepted an unbound parameter"),
    }

    // A multi-row result streams: rows arrive in batches, so peak memory
    // does not grow with the size of the result.
    println!("\nheaviest first:");
    let mut rows = conn.query("SELECT name, grams FROM fruit ORDER BY grams DESC")?;
    while let Some(row) = rows.next_row()? {
        println!("  {:<8} {:>4} g", row.get::<String>(0)?, row.get::<i64>(1)?);
    }

    Ok(())
}
