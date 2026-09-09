// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! A full create-read-update-delete cycle, including an explicit
//! transaction and the rows-affected count.
//!
//! Run with: `cargo run --example crud`

use std::error::Error;

use sqlite_rs::api::{Connection, TransactionBehavior, Value};

fn main() -> Result<(), Box<dyn Error>> {
    // `open` creates the database if the path has no file yet, so no empty
    // fixture needs copying into place first.
    let dir = std::env::temp_dir().join(format!("sqlite-rs-crud-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("crud.db");

    let conn = Connection::open(&path)?;
    println!("opened {} (created it if absent)", path.display());

    // Durability is a choice; FULL fsyncs before a commit returns.
    conn.pragma("synchronous", "FULL")?;

    conn.execute("CREATE TABLE task(id INTEGER PRIMARY KEY, title TEXT, done INTEGER)")?;

    // CREATE, inside a transaction so the three rows land as one unit.
    let insert = conn.prepare("INSERT INTO task(title, done) VALUES (?1, ?2)")?;
    let tx = conn.transaction_with(TransactionBehavior::Immediate)?;
    for title in ["write the spec", "review the PR", "ship it"] {
        insert.execute(vec![Value::from(title), Value::from(false)])?;
        // Rowids are assigned by the engine; `last_insert_rowid` is how a
        // caller learns the one it just wrote.
        println!("inserted {title:?} as rowid {}", tx.last_insert_rowid()?);
    }
    tx.commit()?;

    // READ.
    println!("\nall tasks:");
    let mut rows = conn.query("SELECT id, title, done FROM task ORDER BY id")?;
    while let Some(row) = rows.next_row()? {
        let done: bool = row.get_by_name("done")?;
        println!(
            "  [{}] {} {}",
            if done { 'x' } else { ' ' },
            row.get::<i64>(0)?,
            row.get::<String>(1)?
        );
    }

    // UPDATE. The returned count is what distinguishes a match from a
    // miss — every optimistic-concurrency scheme is built on it.
    let changed = conn.execute_with(
        "UPDATE task SET done = ?1 WHERE title = ?2",
        vec![Value::from(true), Value::from("review the PR")],
    )?;
    println!("\nmarked done: {changed} row(s) changed");

    let missed = conn.execute_with(
        "UPDATE task SET done = ?1 WHERE title = ?2",
        vec![Value::from(true), Value::from("no such task")],
    )?;
    println!("no such task: {missed} row(s) changed");

    // DELETE.
    let deleted = conn.execute("DELETE FROM task WHERE done = 1")?;
    println!("deleted {deleted} completed task(s)");

    // A transaction dropped without committing rolls back.
    {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM task")?;
        println!("\ninside the transaction, {} task(s) remain", count(&tx)?);
        // No `commit()`: dropping here undoes the delete.
    }
    println!("after the rollback, {} task(s) remain", count(&conn)?);

    std::fs::remove_dir_all(&dir).ok();
    Ok(())
}

fn count(conn: &Connection) -> Result<i64, Box<dyn Error>> {
    let row = conn
        .query_row("SELECT count(*) FROM task")?
        .ok_or("count() returned no row")?;
    Ok(row.get(0)?)
}
