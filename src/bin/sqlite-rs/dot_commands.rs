// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Handlers for the REPL's `.help`/`.version`/`.schema`/`.dump`/
//! `.databases`/`.indices` dot-commands (#495) — `.tables`/`.quit`/
//! `.exit` stay in `repl.rs` (#478 already wired those), and
//! `.headers`/`.mode` are pure session-state flips handled inline in
//! `repl.rs` itself (nothing to read from the database for those two).
//! Kept in their own module so `repl.rs`'s dispatch loop doesn't drown
//! in per-command bodies.

use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::dump::dump_database;
use sqlite_rs::format::{format_blob, format_real};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::record::Value;
use sqlite_rs::schema::{read_schema, read_views};
use sqlite_rs::vfs::UnixVfs;

use crate::tables::print_table_names;

/// Every dot-command this REPL recognizes, in `.help` listing order —
/// `.tables`/`.quit`/`.exit` (#478) included, even though their
/// handlers live in `repl.rs`, so `.help` stays the one place that
/// enumerates the whole surface.
const HELP_ENTRIES: &[(&str, &str)] = &[
    (".color on|off", "Turn syntax-highlighted input on or off"),
    (".databases", "List attached databases"),
    (".dump [TABLE]", "Render the database (or one TABLE) as SQL"),
    (".exit", "Exit this program"),
    (".headers on|off", "Turn display of headers on or off"),
    (".help", "Show this message"),
    (
        ".indices [TABLE]",
        "Show names of indexes (of TABLE, if given)",
    ),
    (".mode MODE", "Set output mode: csv column line list"),
    (".quit", "Exit this program"),
    (
        ".schema [TABLE]",
        "Show the CREATE statements (of TABLE, if given)",
    ),
    (".tables [PATTERN]", "List names of tables matching PATTERN"),
    (".version", "Show sqlite-rs and SQLite version info"),
];

pub(crate) fn print_help() {
    for (cmd, desc) in HELP_ENTRIES {
        println!("{cmd:<20}{desc}");
    }
}

/// The SQLite on-disk file-format version this crate targets/emulates —
/// the same 3.53.4 pin recorded in `Cargo.toml`'s `[package.metadata.oracle]`
/// and `tests/corpus/oracle.rs::ORACLE_VERSION`, and what
/// `sqlite_version()` (`src/vdbe/functions.rs`) reports to SQL callers.
/// Duplicated here as a literal rather than plumbed through, matching
/// this crate's existing convention of citing the pin in a comment
/// rather than adding a shared constant across crate boundaries — see
/// `tools/version_pin.py`'s `make version-pin` gate, which holds all of
/// these to the same value.
const SQLITE_FORMAT_VERSION: &str = "3.53.4";

pub(crate) fn print_version() {
    println!("sqlite-rs {}", env!("CARGO_PKG_VERSION"));
    println!("SQLite format {SQLITE_FORMAT_VERSION}");
}

/// `.databases`: this crate has no ATTACH/multi-database support (#495's
/// premise), so the listing is always the trivial one row `sqlite3
/// .databases` would print for a database with nothing attached —
/// `seq|name|file`, `file` an absolute path when it can be resolved.
pub(crate) fn print_databases(path: &Path) {
    let file = std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    println!("0|main|{file}");
}

fn schema_error(e: impl std::fmt::Display) {
    eprintln!("Error: {e}");
}

/// `.schema [TABLE]`: prints stored `CREATE ...` text verbatim (each
/// followed by `;`) — every table (with its indexes) and every view
/// when `table` is `None`, else just the named table's own statement
/// plus its indexes (and a same-named view's statement, if any).
/// Auto-indexes (`sqlite_master.sql IS NULL` — see `TableSchema::indexes`'s
/// doc comment) are silently skipped, same as `sqlite3` does.
///
/// Approximation: real `sqlite3 .schema` orders every object by name
/// (tables/views/indexes interleaved); this instead groups each table
/// with its own indexes, then views — simpler, and every individual
/// statement byte-matches the oracle, but the overall ordering of
/// unrelated tables' schemas is not guaranteed to match rowid/name
/// order.
pub(crate) fn print_schema(
    pager: &Rc<std::cell::RefCell<Pager>>,
    header: &DatabaseHeader,
    table: Option<&str>,
) {
    let borrowed = pager.borrow();
    let mut schema_cursor = TableCursor::new(&*borrowed, header, 1);
    let schemas = match read_schema(&mut schema_cursor, header.text_encoding) {
        Ok(s) => s,
        Err(e) => return schema_error(e),
    };
    let mut view_cursor = TableCursor::new(&*borrowed, header, 1);
    let views = match read_views(&mut view_cursor, header.text_encoding) {
        Ok(v) => v,
        Err(e) => return schema_error(e),
    };
    drop(borrowed);

    for schema in &schemas {
        if let Some(t) = table {
            if schema.name != t {
                continue;
            }
        }
        if !schema.sql.is_empty() {
            println!("{};", schema.sql);
        }
        // Index `CREATE` text lives on the index's own `sqlite_master`
        // row, not on `IndexSchema` (which only keeps the naively-parsed
        // column list) — re-walk `sqlite_master` directly for this
        // table's index `sql` text, grouping each table with its own
        // indexes.
        print_index_sql(pager, header, &schema.name);
    }

    if table.is_none() {
        for view in &views {
            if !view.sql.is_empty() {
                println!("{};", view.sql);
            }
        }
    } else if let Some(view) = views.iter().find(|v| Some(v.name.as_str()) == table) {
        println!("{};", view.sql);
    }
}

/// Walks `sqlite_master` directly for `type = 'index'` rows with
/// non-NULL `sql` text (auto-indexes have none) belonging to `table`,
/// printed for `.schema`'s table+indexes grouping.
fn print_index_sql(pager: &Rc<std::cell::RefCell<Pager>>, header: &DatabaseHeader, table: &str) {
    let borrowed = pager.borrow();
    let mut cursor = TableCursor::new(&*borrowed, header, 1);
    let Ok(mut row) = cursor.first_row() else {
        return;
    };
    while let Some(r) = row {
        if let Ok(values) = sqlite_rs::record::decode_record(&r.payload, header.text_encoding) {
            if values.len() == 5 {
                let kind = text(values.first());
                let tbl_name = text(values.get(2));
                let sql = text(values.get(4));
                if kind == "index" && !sql.is_empty() && tbl_name == table {
                    println!("{sql};");
                }
            }
        }
        row = cursor.next_row().unwrap_or(None);
    }
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::Text(s)) => s.to_string(),
        _ => String::new(),
    }
}

/// `.indices [TABLE]`: index names only (not their `sql`), columnized
/// like `.tables` — every index in the database, or just those on
/// `table` when given.
pub(crate) fn print_indices(
    pager: &Rc<std::cell::RefCell<Pager>>,
    header: &DatabaseHeader,
    table: Option<&str>,
) {
    let borrowed = pager.borrow();
    let mut schema_cursor = TableCursor::new(&*borrowed, header, 1);
    let schemas = match read_schema(&mut schema_cursor, header.text_encoding) {
        Ok(s) => s,
        Err(e) => return schema_error(e),
    };
    drop(borrowed);

    let mut names: Vec<String> = schemas
        .iter()
        .filter(|s| table.is_none_or(|t| s.name == t))
        .flat_map(|s| s.indexes.iter().map(|i| i.name.clone()))
        .collect();
    names.sort_unstable();
    print_table_names(&names);
}

/// A SQL-literal rendering of `v`, for `.dump`'s `INSERT INTO ...
/// VALUES(...)` statements — `sqlite3 .dump`'s `quote()`-style output:
/// `NULL`, a bare number, a single-quoted (embedded `'` doubled) text
/// literal, or an `X'HEX'` blob literal.
fn sql_literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format_real(*r),
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Blob(b) => format_blob(b),
    }
}

/// `.dump [TABLE]`: valid SQL re-derived from the database's current
/// on-disk state (a fresh `dump_database` re-open, same as the `dump`
/// subcommand — it does not see this REPL session's own uncommitted
/// writes, unlike `.schema`/`.indices` above, which read through the
/// session's live `Pager`) — `BEGIN TRANSACTION;`, each table's `CREATE`
/// statement plus one `INSERT INTO ... VALUES(...)` per row, `COMMIT;`.
pub(crate) fn print_dump(path: &Path, table: Option<&str>) {
    let result = match dump_database(&UnixVfs, path) {
        Ok(r) => r,
        Err(e) => return schema_error(e),
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if let Err(e) = write_dump(&mut out, &result, table) {
        return schema_error(e);
    }
    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }
}

fn write_dump(
    out: &mut impl Write,
    result: &sqlite_rs::dump::DumpResult,
    table: Option<&str>,
) -> io::Result<()> {
    writeln!(out, "BEGIN TRANSACTION;")?;
    for t in &result.tables {
        if let Some(name) = table {
            if t.name != name {
                continue;
            }
        }
        if !t.sql.is_empty() {
            writeln!(out, "{};", t.sql)?;
        }
        for row in &t.rows {
            let values: Vec<String> = row.iter().map(sql_literal).collect();
            writeln!(
                out,
                "INSERT INTO {} VALUES({});",
                quote_ident(&t.name),
                values.join(",")
            )?;
        }
    }
    writeln!(out, "COMMIT;")
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
