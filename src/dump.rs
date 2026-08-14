//! Whole-database read: schema + every row of every table, the shared
//! core behind the `dump` and `export` CLI subcommands (issue #37, V1
//! step 9 — the acceptance gate for epic #5).
//!
//! Opening a database or reading its schema is a hard failure (the file
//! can't safely be read at all). A single table failing to decode is a
//! "graceful unknown" (spec 001-architecture Requirement 4): it's
//! skipped, a warning is recorded, and every other table still comes
//! out. Virtual tables (no b-tree storage of their own) are always
//! skipped with a warning, never attempted.

use std::path::Path;

use thiserror::Error;

use crate::btree::{BtreeError, IndexCursor, IndexRow, TableCursor, TableRow};
use crate::header::{DatabaseHeader, HeaderError, HEADER_LEN};
use crate::pager::{Pager, PagerError};
use crate::record::{decode_record, RecordError, TextEncoding, Value};
use crate::schema::{read_schema, DdlError, TableSchema};
use crate::vfs::{PageSource, Vfs, VfsError};

#[derive(Debug, Error)]
pub enum DumpError {
    #[error(transparent)]
    Vfs(#[from] VfsError),

    #[error("parsing database header: {0}")]
    Header(#[from] HeaderError),

    #[error("opening pager: {0}")]
    Pager(#[from] PagerError),

    #[error("reading schema: {0}")]
    Schema(#[from] DdlError),
}

/// One table's decoded rows, ready for `-list`/`-csv` rendering. The
/// rowid-alias column (a lone `INTEGER PRIMARY KEY` column, whose value
/// is never actually stored in the record — see spike 003 finding 1)
/// has already been substituted with the row's real rowid.
pub struct TableDump {
    pub name: String,
    pub sql: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Every readable table in a database, plus warnings for anything
/// skipped. `tables` preserves `sqlite_master` order.
pub struct DumpResult {
    pub tables: Vec<TableDump>,
    pub warnings: Vec<String>,
}

/// Opens `path` through `vfs`, parses its header, and returns a
/// [`Pager`] over it. Shared by both `dump_database` and any caller that
/// needs the same open path (e.g. re-opening per table, since [`Pager`]
/// isn't [`Clone`] and this crate's own tests establish "open fresh per
/// cursor" as the pattern — see `src/pager/mod.rs`'s fixture tests).
pub fn open<V: Vfs>(vfs: &V, path: &Path) -> Result<(DatabaseHeader, Pager), DumpError> {
    let file = vfs.open_read(path)?;
    let mut header_buf = [0u8; HEADER_LEN];
    file.read_at(&mut header_buf, 0)?;
    let header = DatabaseHeader::parse(&header_buf)?;
    let pager = Pager::open(vfs, path, header.page_size)?;
    Ok((header, pager))
}

/// Reads every table's schema and rows out of the database at `path`.
pub fn dump_database<V: Vfs>(vfs: &V, path: &Path) -> Result<DumpResult, DumpError> {
    let (header, pager) = open(vfs, path)?;
    let mut schema_cursor = TableCursor::new(pager, &header, 1);
    let schemas = read_schema(&mut schema_cursor, header.text_encoding)?;

    let mut tables = Vec::new();
    let mut warnings = Vec::new();

    for schema in &schemas {
        if schema.is_virtual {
            warnings.push(format!(
                "table {:?}: virtual table, no storage of its own — skipped",
                schema.name
            ));
            continue;
        }
        let (_, pager) = match open(vfs, path) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!("table {:?}: reopening database: {e}", schema.name));
                continue;
            }
        };
        match read_table_rows(pager, &header, schema) {
            Ok(rows) => tables.push(TableDump {
                name: schema.name.clone(),
                sql: schema.sql.clone(),
                columns: schema.columns.clone(),
                rows,
            }),
            Err(e) => warnings.push(format!("table {:?}: {e}", schema.name)),
        }
    }

    Ok(DumpResult { tables, warnings })
}

/// Splits a `CREATE TABLE ...(col-defs)...` statement's column-definition
/// list into raw per-column definition strings, in declared order —
/// deliberately re-derived from `schema.sql` here (rather than exposed
/// from `src/schema`) since only this module needs per-column type text,
/// not just names. Mirrors `src/schema/ddl_reader.rs`'s own naive
/// top-level-comma splitter and table-constraint filter.
fn column_defs(schema: &TableSchema) -> Vec<&str> {
    let Some(start) = schema.sql.find('(') else {
        return Vec::new();
    };
    let mut depth = 0i32;
    let mut end = None;
    for (i, c) in schema.sql[start..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return Vec::new();
    };
    let inner = &schema.sql[start + 1..end];

    let mut depth = 0i32;
    let mut part_start = 0usize;
    let mut defs = Vec::new();
    let bytes = inner.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                defs.push(inner[part_start..i].trim());
                part_start = i + 1;
            }
            _ => {}
        }
    }
    defs.push(inner[part_start..].trim());

    defs.into_iter()
        .filter(|def| {
            let upper = def.to_ascii_uppercase();
            !(upper.starts_with("PRIMARY KEY")
                || upper.starts_with("UNIQUE")
                || upper.starts_with("FOREIGN KEY")
                || upper.starts_with("CHECK")
                || upper.starts_with("CONSTRAINT"))
        })
        .collect()
}

/// The one-column special case SQLite calls the rowid alias: a table
/// declared with a single `INTEGER PRIMARY KEY` column (not `WITHOUT
/// ROWID`) stores that column as a NULL placeholder in every record and
/// expects the reader to substitute the cursor's own rowid instead (see
/// `src/btree/mod.rs`'s module doc and spike 003 finding 1). Returns the
/// 0-based column index to substitute, if any.
fn rowid_alias_column(schema: &TableSchema) -> Option<usize> {
    if schema.without_rowid {
        return None;
    }
    for (idx, def) in column_defs(schema).iter().enumerate() {
        let upper = def.to_ascii_uppercase();
        let is_int_pk = upper
            .split(|c: char| !c.is_alphanumeric())
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w == ["PRIMARY", "KEY"])
            && upper.split_whitespace().any(|w| w == "INTEGER");
        if is_int_pk {
            return Some(idx);
        }
    }
    None
}

/// Column indices with REAL type affinity (declared type containing
/// `REAL`, `FLOA`, or `DOUB`, per SQLite's type-affinity rules). Needed
/// because SQLite's serial-type-8/9 space optimization ("the constant
/// integer 0 or 1") applies to a REAL column storing exactly `0.0`/`1.0`
/// too — the raw record alone can't tell those apart from a genuine
/// INTEGER 0/1 without the column's declared type. Confirmed empirically
/// against `sqlite3 -list` (a REAL column holding `0.0` renders `0.0`,
/// not `0`).
fn real_affinity_columns(schema: &TableSchema) -> Vec<bool> {
    column_defs(schema)
        .iter()
        .map(|def| {
            let upper = def.to_ascii_uppercase();
            let declared_type = upper
                .split_once(char::is_whitespace)
                .map(|(_, rest)| rest)
                .unwrap_or("");
            declared_type.contains("REAL")
                || declared_type.contains("FLOA")
                || declared_type.contains("DOUB")
        })
        .collect()
}

fn apply_real_affinity(values: &mut [Value], real_affinity: &[bool]) {
    for (i, is_real) in real_affinity.iter().enumerate() {
        if !is_real {
            continue;
        }
        if let Some(v) = values.get_mut(i) {
            match v {
                Value::Integer(0) => *v = Value::Real(0.0),
                Value::Integer(1) => *v = Value::Real(1.0),
                _ => {}
            }
        }
    }
}

fn read_table_rows<P: PageSource>(
    source: P,
    header: &DatabaseHeader,
    schema: &TableSchema,
) -> Result<Vec<Vec<Value>>, TableReadError> {
    let alias_col = rowid_alias_column(schema);
    let real_affinity = real_affinity_columns(schema);
    if schema.without_rowid {
        let mut cursor = IndexCursor::new(source, header.usable_page_size(), schema.root_page);
        let mut rows = Vec::new();
        let mut row = cursor.first()?;
        while let Some(r) = row {
            let mut values = decode_index_row(&r, header.text_encoding)?;
            apply_real_affinity(&mut values, &real_affinity);
            rows.push(values);
            row = cursor.next()?;
        }
        Ok(rows)
    } else {
        let mut cursor = TableCursor::new(source, header, schema.root_page);
        let mut rows = Vec::new();
        let mut row = cursor.first()?;
        while let Some(r) = row {
            let mut values = decode_table_row(&r, header.text_encoding, alias_col)?;
            apply_real_affinity(&mut values, &real_affinity);
            rows.push(values);
            row = cursor.next()?;
        }
        Ok(rows)
    }
}

fn decode_table_row(
    row: &TableRow,
    encoding: TextEncoding,
    alias_col: Option<usize>,
) -> Result<Vec<Value>, TableReadError> {
    let mut values = decode_record(&row.payload, encoding)?;
    if let Some(idx) = alias_col {
        if let Some(v) = values.get_mut(idx) {
            *v = Value::Integer(row.rowid);
        }
    }
    Ok(values)
}

fn decode_index_row(row: &IndexRow, encoding: TextEncoding) -> Result<Vec<Value>, TableReadError> {
    Ok(decode_record(&row.payload, encoding)?)
}

#[derive(Debug, Error)]
enum TableReadError {
    #[error(transparent)]
    Btree(#[from] BtreeError),

    #[error(transparent)]
    Record(#[from] RecordError),
}
