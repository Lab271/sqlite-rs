//! `sqlite_master` decode + minimal DDL reader. Lives outside
//! `src/parser/` by design (spec 002-parser Requirement 5): zero
//! dependency on the future full SQL parser, so schema decoding keeps
//! working with the parser feature-gated off (trivially true today,
//! since no parser module exists yet — re-verify this claim when one
//! lands).
//!
//! Unparseable DDL (e.g. a virtual table) degrades to raw-row access
//! (`columns: vec![]`), never an error — Tier 0's "graceful unknowns"
//! (spec 001-architecture Requirement 4).
//!
//! The DDL parser here is deliberately naive, matching spike 005 (#12)'s
//! prototype: no quoted/bracketed identifiers with embedded whitespace,
//! no dialect edge cases beyond what the real corpus fixtures exercise.
//! "Nothing more" than table name, column names, declared types (via
//! column name extraction only — types are not separately captured),
//! and WITHOUT ROWID / STRICT markers, per the originating issue's scope.

use thiserror::Error;

use crate::btree::{BtreeError, TableCursor};
use crate::record::{decode_record, RecordError, TextEncoding, Value};
use crate::vfs::PageSource;

#[derive(Debug, Error)]
pub enum DdlError {
    #[error("walking sqlite_master: {0}")]
    Btree(#[from] BtreeError),

    #[error("decoding a sqlite_master row: {0}")]
    Record(#[from] RecordError),

    #[error("sqlite_master row has {0} columns, expected 5")]
    MalformedRow(usize),
}

/// A minimally-parsed table schema entry: everything Tier 0 needs to
/// read a table without the full SQL parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    pub name: String,
    pub root_page: u32,
    pub columns: Vec<String>,
    pub without_rowid: bool,
    pub strict: bool,
    /// `CREATE VIRTUAL TABLE ...` — DDL this reader deliberately does not
    /// parse. `columns` is always empty and `root_page` is `0` (virtual
    /// tables have no b-tree storage of their own).
    pub is_virtual: bool,
    /// The raw `sql` column text from `sqlite_master` — the verbatim
    /// `CREATE TABLE`/`CREATE VIRTUAL TABLE` statement, needed by callers
    /// that reproduce schema DDL verbatim (e.g. a `dump` CLI).
    pub sql: String,
}

/// Walks `sqlite_master` via `cursor` (which callers MUST construct with
/// `root_page = 1`) and returns every `type = 'table'` entry as a
/// [`TableSchema`]. Never errors on unparseable or virtual-table DDL —
/// those degrade to `columns: vec![]`.
pub fn read_schema<P: PageSource>(
    cursor: &mut TableCursor<P>,
    encoding: TextEncoding,
) -> Result<Vec<TableSchema>, DdlError> {
    let mut schemas = Vec::new();
    let mut row = cursor.first()?;
    while let Some(r) = row {
        let values = decode_record(&r.payload, encoding)?;
        if values.len() != 5 {
            return Err(DdlError::MalformedRow(values.len()));
        }
        if text(values.first()) == "table" {
            schemas.push(table_schema(&values));
        }
        row = cursor.next()?;
    }
    Ok(schemas)
}

fn table_schema(values: &[Value]) -> TableSchema {
    let name = text(values.get(1)).to_string();
    let root_page = match values.get(3) {
        Some(Value::Integer(i)) => *i as u32,
        _ => 0,
    };
    let sql = text(values.get(4));

    if is_virtual_table(sql) {
        return TableSchema {
            name,
            root_page: 0,
            columns: Vec::new(),
            without_rowid: false,
            strict: false,
            is_virtual: true,
            sql: sql.to_string(),
        };
    }

    let parsed = parse_create_table(sql).unwrap_or_default();
    TableSchema {
        name,
        root_page,
        columns: parsed.columns,
        without_rowid: parsed.without_rowid,
        strict: parsed.strict,
        is_virtual: false,
        sql: sql.to_string(),
    }
}

fn text(v: Option<&Value>) -> &str {
    match v {
        Some(Value::Text(s)) => s,
        _ => "",
    }
}

fn is_virtual_table(sql: &str) -> bool {
    sql.trim_start()
        .to_ascii_uppercase()
        .starts_with("CREATE VIRTUAL TABLE")
}

#[derive(Default)]
struct ParsedCreateTable {
    columns: Vec<String>,
    without_rowid: bool,
    strict: bool,
}

/// Parses `CREATE TABLE ... (col-defs) [table-options]`. Returns `None`
/// for anything this naive reader can't find a column list in — the
/// caller treats that identically to a virtual table (empty columns,
/// never an error).
fn parse_create_table(sql: &str) -> Option<ParsedCreateTable> {
    let start = sql.find('(')?;
    let mut depth = 0i32;
    let mut end = None;
    for (i, c) in sql.get(start..)?.char_indices() {
        match c {
            '(' => depth = depth.saturating_add(1),
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = Some(start.saturating_add(i));
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let inner = sql.get(start.saturating_add(1)..end)?;
    let trailer = sql.get(end.saturating_add(1)..)?.to_ascii_uppercase();

    let columns = split_top_level_commas(inner)
        .into_iter()
        .filter(|def| !is_table_constraint(def))
        .map(|def| column_name(&def))
        .collect();

    Some(ParsedCreateTable {
        columns,
        without_rowid: trailer.contains("WITHOUT ROWID"),
        strict: trailer.contains("STRICT"),
    })
}

/// A standalone table-level constraint (`PRIMARY KEY(...)`, `UNIQUE(...)`,
/// `FOREIGN KEY(...)`, `CHECK(...)`, `CONSTRAINT name ...`) rather than a
/// column definition. Inline column-level constraints (e.g. `k PRIMARY
/// KEY`) don't start with the keyword — the column name comes first —
/// so this check doesn't false-positive on those.
fn is_table_constraint(def: &str) -> bool {
    let upper = def.trim().to_ascii_uppercase();
    upper.starts_with("PRIMARY KEY")
        || upper.starts_with("UNIQUE")
        || upper.starts_with("FOREIGN KEY")
        || upper.starts_with("CHECK")
        || upper.starts_with("CONSTRAINT")
}

fn split_top_level_commas(inner: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut part_start = 0usize;
    let bytes = inner.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth = depth.saturating_add(1),
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(inner.get(part_start..i).unwrap_or("").trim().to_string());
                part_start = i.saturating_add(1);
            }
            _ => {}
        }
    }
    parts.push(inner.get(part_start..).unwrap_or("").trim().to_string());
    parts
}

fn column_name(def: &str) -> String {
    def.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(['"', '`', '['].as_ref())
        .trim_matches([']'].as_ref())
        .to_string()
}

/// Splits a `CREATE TABLE ...(col-defs)...` statement's column-definition
/// list into raw per-column definition strings, in declared order —
/// re-derived from `schema.sql` rather than kept alongside `columns`,
/// which holds names only — `src/dump.rs` needs each column's declared
/// type text, and [`rowid_alias_column`] needs its full constraint
/// text. Mirrors this module's own naive top-level-comma splitter and
/// table-constraint filter.
pub(crate) fn column_defs(schema: &TableSchema) -> Vec<&str> {
    let Some(start) = schema.sql.find('(') else {
        return Vec::new();
    };
    let mut depth = 0i32;
    let mut end = None;
    for (i, c) in schema.sql[start..].char_indices() {
        match c {
            '(' => depth = depth.saturating_add(1),
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = Some(start.saturating_add(i));
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return Vec::new();
    };
    let inner = &schema.sql[start.saturating_add(1)..end];

    let mut depth = 0i32;
    let mut part_start = 0usize;
    let mut defs = Vec::new();
    let bytes = inner.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth = depth.saturating_add(1),
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                defs.push(inner[part_start..i].trim());
                part_start = i.saturating_add(1);
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
pub fn rowid_alias_column(schema: &TableSchema) -> Option<usize> {
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use crate::header::DatabaseHeader;
    use crate::vfs::{UnixVfs, Vfs, VfsPageSource};
    use std::path::Path;

    fn read_fixture(family: &str, name: &str) -> Vec<TableSchema> {
        let path = Path::new("tests/corpus/fixtures").join(family).join(name);
        let vfs = UnixVfs;
        let file = vfs
            .open_read(&path)
            .unwrap_or_else(|e| panic!("open {path:?}: {e}"));
        let mut header_buf = [0u8; 100];
        file.read_at(&mut header_buf, 0).unwrap();
        let header = DatabaseHeader::parse(&header_buf).unwrap();
        let source = VfsPageSource::open(&vfs, &path, header.page_size).unwrap();
        let mut cursor = TableCursor::new(source, &header, 1);
        read_schema(&mut cursor, header.text_encoding).unwrap()
    }

    fn find(schemas: &[TableSchema], name: &str) -> TableSchema {
        schemas
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("no table named {name} in {schemas:?}"))
    }

    #[test]
    fn plain_table_columns_and_rootpage() {
        let schemas = read_fixture("btrees", "table_single_page.db");
        assert_eq!(schemas.len(), 1);
        let t = find(&schemas, "t");
        assert_eq!(t.columns, vec!["a", "b"]);
        assert_eq!(t.root_page, 2);
        assert!(!t.without_rowid);
        assert!(!t.strict);
        assert!(!t.is_virtual);
    }

    #[test]
    fn without_rowid_marker_detected() {
        let schemas = read_fixture("btrees", "without_rowid.db");
        let t = find(&schemas, "t");
        assert_eq!(t.columns, vec!["k", "v"]);
        assert!(t.without_rowid);
    }

    #[test]
    fn strict_marker_detected_and_generated_column_named_correctly() {
        let schemas = read_fixture("features", "strict_generated.db");
        let t = find(&schemas, "t");
        // `b INTEGER GENERATED ALWAYS AS (a*2) STORED` — the naive
        // column-name extractor must not be confused by the nested
        // parens in the generated-column expression.
        assert_eq!(t.columns, vec!["a", "b"]);
        assert!(t.strict);
        assert!(!t.without_rowid);
    }

    #[test]
    fn autovacuum_fixture_plain_table_unaffected_by_pointer_map_page() {
        let schemas = read_fixture("features", "autovacuum.db");
        let t = find(&schemas, "t");
        assert_eq!(t.columns, vec!["a", "b"]);
    }

    #[test]
    fn fts5_virtual_table_is_graceful_unknown_shadow_tables_are_readable() {
        let schemas = read_fixture("features", "fts5.db");

        let virtual_entry = find(&schemas, "t");
        assert!(virtual_entry.is_virtual);
        assert!(virtual_entry.columns.is_empty());
        assert_eq!(virtual_entry.root_page, 0);

        // Shadow tables are ordinary sqlite_master table entries and
        // must be fully readable, including the WITHOUT ROWID ones
        // (spike 005, #12's finding that motivated spec 006 Req 5/6).
        assert_eq!(find(&schemas, "t_data").columns, vec!["id", "block"]);
        assert!(!find(&schemas, "t_data").without_rowid);

        let t_idx = find(&schemas, "t_idx");
        // `(segid, term, pgno, PRIMARY KEY(segid, term))` — the
        // standalone table-level PRIMARY KEY(...) constraint must not be
        // mistaken for a 4th column named "PRIMARY".
        assert_eq!(t_idx.columns, vec!["segid", "term", "pgno"]);
        assert!(t_idx.without_rowid);

        assert_eq!(find(&schemas, "t_content").columns, vec!["id", "c0"]);
        assert_eq!(find(&schemas, "t_docsize").columns, vec!["id", "sz"]);

        let t_config = find(&schemas, "t_config");
        // `(k PRIMARY KEY, v)` — inline column-level PRIMARY KEY on `k`
        // must still name the column "k", not be filtered out.
        assert_eq!(t_config.columns, vec!["k", "v"]);
        assert!(t_config.without_rowid);
    }

    #[test]
    fn rtree_virtual_table_is_graceful_unknown_shadow_tables_are_readable() {
        let schemas = read_fixture("features", "rtree.db");

        let virtual_entry = find(&schemas, "t");
        assert!(virtual_entry.is_virtual);
        assert!(virtual_entry.columns.is_empty());

        assert_eq!(find(&schemas, "t_rowid").columns, vec!["rowid", "nodeno"]);
        assert_eq!(find(&schemas, "t_node").columns, vec!["nodeno", "data"]);
        assert_eq!(
            find(&schemas, "t_parent").columns,
            vec!["nodeno", "parentnode"]
        );
    }

    #[test]
    fn unparseable_non_virtual_ddl_degrades_gracefully_never_errors() {
        // Synthetic row: no column-list parens at all. Not a real
        // fixture case (every real CREATE TABLE has a paren list), but
        // exercises the "never an error" guarantee for anything this
        // naive reader genuinely can't make sense of.
        let values = vec![
            Value::Text("table".to_string()),
            Value::Text("weird".to_string()),
            Value::Text("weird".to_string()),
            Value::Integer(5),
            Value::Text("garbage with no parens".to_string()),
        ];
        let schema = table_schema(&values);
        assert_eq!(schema.name, "weird");
        assert_eq!(schema.root_page, 5);
        assert!(schema.columns.is_empty());
        assert!(!schema.is_virtual);
    }

    #[test]
    fn non_table_entries_are_excluded() {
        // index.db has an explicit secondary index (idx_b) alongside
        // table t — read_schema must return only the table.
        let schemas = read_fixture("btrees", "index.db");
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "t");
    }
}
