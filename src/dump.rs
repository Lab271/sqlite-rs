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
use crate::schema::{column_defs, read_schema, rowid_alias_column, DdlError, TableSchema};
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

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(sql: &str) -> TableSchema {
        TableSchema {
            name: "t".to_string(),
            root_page: 1,
            columns: vec![],
            column_types: vec![],
            without_rowid: false,
            strict: false,
            is_virtual: false,
            sql: sql.to_string(),
        }
    }

    #[test]
    fn rowid_alias_detects_plain_integer_primary_key() {
        let s = schema("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)");
        assert_eq!(rowid_alias_column(&s), Some(0));
    }

    #[test]
    fn rowid_alias_none_for_without_rowid() {
        let mut s = schema("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)");
        s.without_rowid = true;
        assert_eq!(rowid_alias_column(&s), None);
    }

    #[test]
    fn real_affinity_detects_declared_real_types() {
        let s = schema("CREATE TABLE t (a REAL, b DOUBLE, c FLOAT, d INTEGER)");
        assert_eq!(real_affinity_columns(&s), vec![true, true, true, false]);
    }

    // Known-fragile cases: `column_defs`/`rowid_alias_column`/
    // `real_affinity_columns` re-derive column info from raw DDL text
    // with naive splitting rather than the structured parser in
    // `src/schema/ddl_reader.rs`. These tests document current (not
    // necessarily correct) behavior so a future fix has a baseline —
    // see PR #49 review discussion.

    #[test]
    fn known_fragile_comma_inside_string_literal_default_missplits_columns() {
        // `DEFAULT 'a,b'` contains a comma that isn't a column separator,
        // but `column_defs` has no string-literal awareness and splits on
        // every top-level comma.
        let s = schema("CREATE TABLE t (a TEXT DEFAULT 'a,b', b INTEGER)");
        assert_eq!(
            column_defs(&s).len(),
            3,
            "comma inside the string literal was wrongly treated as a column separator"
        );
    }

    #[test]
    fn known_fragile_constraint_text_can_false_positive_on_affinity_keywords() {
        // A CHECK constraint mentioning "FLOAT" as a string, not a type,
        // still makes `real_affinity_columns` treat the column as REAL —
        // because it substring-matches the whole remainder after the
        // column name, not just the declared-type token.
        let s = schema("CREATE TABLE t (a TEXT CHECK(a != 'FLOAT'))");
        assert_eq!(
            real_affinity_columns(&s),
            vec![true],
            "constraint text mentioning FLOAT was wrongly detected as REAL affinity"
        );
    }

    // The two `rowid_alias_column` cases below are the reason that
    // function's naivety now carries more weight than it did: since #96,
    // `src/codegen/expr.rs` emits `Rowid` instead of `Column` based on
    // its answer, so a wrong index is a wrong query result rather than
    // only wrong `dump` output. Both are tracked for a quote-aware
    // rewrite in #135; these tests pin today's behavior so the fix is
    // visible when it lands.

    #[test]
    fn known_fragile_string_literal_mentioning_primary_key_false_positives() {
        // The DEFAULT literal supplies the PRIMARY/KEY token pair and the
        // column supplies INTEGER, so this reads as a rowid alias and the
        // compiled read path would substitute the cursor rowid for `a`.
        let s = schema("CREATE TABLE t (a INTEGER DEFAULT 'primary key', b TEXT)");
        assert_eq!(
            rowid_alias_column(&s),
            Some(0),
            "PRIMARY KEY inside a string literal was wrongly read as a real constraint"
        );
    }

    #[test]
    fn known_fragile_table_level_primary_key_misses_the_alias() {
        // SQLite treats `CREATE TABLE t(x INTEGER, PRIMARY KEY(x))` as a
        // rowid alias, but the table-constraint filter drops that def
        // before the scan sees it — so `x` reads back NULL, which is the
        // original #96 bug surviving for this DDL spelling.
        let s = schema("CREATE TABLE t (x INTEGER, PRIMARY KEY(x))");
        assert_eq!(
            rowid_alias_column(&s),
            None,
            "table-level PRIMARY KEY(x) over an INTEGER column is a rowid alias in SQLite"
        );
    }

    #[test]
    fn rowid_alias_none_for_integer_primary_key_desc() {
        // Not fragile — a real rule: the DESC form gets its own b-tree
        // index and stores the column normally, so it must NOT be
        // substituted (SQLite's "ROWIDs and the INTEGER PRIMARY KEY").
        let s = schema("CREATE TABLE t (id INTEGER PRIMARY KEY DESC, name TEXT)");
        assert_eq!(rowid_alias_column(&s), None);
    }
}
