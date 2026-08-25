//! `sqlite_master` decode + minimal DDL reader. Lives outside
//! `src/parser/` by design (spec 002-parser Requirement 5): zero
//! dependency on the full SQL parser, so schema decoding keeps working
//! with the parser feature-gated off. `src/parser/` now exists (and
//! already has a `CREATE INDEX` AST/parser), but this module deliberately
//! does not call into it — see #211.
//!
//! Unparseable DDL (e.g. a virtual table) degrades to raw-row access
//! (`columns: vec![]`), never an error — Tier 0's "graceful unknowns"
//! (spec 001-architecture Requirement 4). The same rule extends to
//! `CREATE INDEX` entries (#211): an index this reader can't find a
//! column list for (e.g. an auto-index, whose `sqlite_master.sql` is
//! `NULL`) is silently omitted from `TableSchema::indexes`.
//!
//! The DDL parser here is deliberately naive, matching spike 005 (#12)'s
//! prototype: no quoted/bracketed identifiers with embedded whitespace,
//! no dialect edge cases beyond what the real corpus fixtures exercise.
//! "Nothing more" than table name, column names, declared types (via
//! column name extraction only — types are not separately captured),
//! WITHOUT ROWID / STRICT markers, and (#211) each table's indexes
//! (column list, ASC/DESC, uniqueness), per the originating issues' scope.

use crate::btree::{BtreeError, TableCursor};
use crate::record::{decode_record, Collation, RecordError, TextEncoding, Value};
use crate::vfs::PageSource;

/// Failure walking or decoding `sqlite_master`.
#[derive(Debug)]
pub enum DdlError {
    /// Failure walking the `sqlite_master` b-tree.
    Btree(BtreeError),

    /// Failure decoding a `sqlite_master` row's record payload.
    Record(RecordError),

    /// A `sqlite_master` row didn't have exactly 5 columns.
    MalformedRow(usize),
}

impl std::fmt::Display for DdlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DdlError::Btree(e) => write!(f, "walking sqlite_master: {e}"),
            DdlError::Record(e) => write!(f, "decoding a sqlite_master row: {e}"),
            DdlError::MalformedRow(n) => {
                write!(f, "sqlite_master row has {n} columns, expected 5")
            }
        }
    }
}

impl std::error::Error for DdlError {}

impl From<BtreeError> for DdlError {
    fn from(e: BtreeError) -> Self {
        DdlError::Btree(e)
    }
}

impl From<RecordError> for DdlError {
    fn from(e: RecordError) -> Self {
        DdlError::Record(e)
    }
}

/// A minimally-parsed table schema entry: everything Tier 0 needs to
/// read a table without the full SQL parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    /// The table's name.
    pub name: String,
    /// The table's b-tree root page (`sqlite_master.rootpage`).
    pub root_page: u32,
    /// Column names, in declared order.
    pub columns: Vec<String>,
    /// Whether the table was declared `WITHOUT ROWID`.
    pub without_rowid: bool,
    /// Whether the table was declared `STRICT`.
    pub strict: bool,
    /// Each column's declared type text, position-for-position with
    /// `columns` (empty string when a column has none) — the
    /// substring `affinity_of` (spec 008 Requirement 1) derives
    /// column affinity from. Comparison-affinity derivation (#138)
    /// needs this; `dump.rs` re-derives its own full column text from
    /// `sql` instead via [`column_defs`], so this is naive on purpose:
    /// same textual scope as `columns`, no dialect edge cases beyond
    /// what the corpus exercises.
    pub column_types: Vec<String>,
    /// Each column's declared `COLLATE` (default [`Collation::Binary`]
    /// when absent), position-for-position with `columns` — the
    /// crate-wide fallback #500 introduces for comparisons that don't
    /// spell out an explicit `COLLATE` in the query itself.
    pub column_collations: Vec<Collation>,
    /// `CREATE VIRTUAL TABLE ...` — DDL this reader deliberately does not
    /// parse. `columns` is always empty and `root_page` is `0` (virtual
    /// tables have no b-tree storage of their own).
    pub is_virtual: bool,
    /// The raw `sql` column text from `sqlite_master` — the verbatim
    /// `CREATE TABLE`/`CREATE VIRTUAL TABLE` statement, needed by callers
    /// that reproduce schema DDL verbatim (e.g. a `dump` CLI).
    pub sql: String,
    /// Every `CREATE INDEX`/`CREATE UNIQUE INDEX` entry in `sqlite_master`
    /// whose `tbl_name` is this table. Auto-indexes created implicitly for
    /// `PRIMARY KEY`/`UNIQUE` column constraints have a `NULL` `sql`
    /// column in `sqlite_master` and are not captured here — same
    /// graceful-degradation rule as unparseable DDL elsewhere in this
    /// reader (#211).
    pub indexes: Vec<IndexSchema>,
}

/// A minimally-parsed `CREATE INDEX` entry, naive in the same sense as
/// [`TableSchema`]: column list and ASC/DESC/uniqueness only, nothing more
/// (#211).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexSchema {
    /// The index's name.
    pub name: String,
    /// Whether the index was declared `UNIQUE`.
    pub unique: bool,
    /// The indexed columns, in declared key order.
    pub columns: Vec<IndexedColumn>,
    /// The index b-tree's root page (`sqlite_master.rootpage`), needed
    /// to `OpenWrite` a write cursor onto it (#196).
    pub root_page: u32,
}

/// One column (or expression, kept as raw text) in an index's key,
/// position-for-position with the index's declared column order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedColumn {
    /// The column's (unquoted) name, or raw expression text for an
    /// expression index.
    pub name: String,
    /// Whether this key part is sorted `DESC`.
    pub desc: bool,
    /// The key part's declared `COLLATE` (default [`Collation::Binary`]
    /// when absent).
    pub collation: Collation,
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
    let mut pending_indexes: Vec<(String, IndexSchema)> = Vec::new();
    let mut row = cursor.first_row()?;
    while let Some(r) = row {
        let values = decode_record(&r.payload, encoding)?;
        if values.len() != 5 {
            return Err(DdlError::MalformedRow(values.len()));
        }
        match text(values.first()) {
            "table" => schemas.push(table_schema(&values)),
            "index" => pending_indexes.extend(index_schema(&values)),
            _ => {}
        }
        row = cursor.next_row()?;
    }
    for (table_name, index) in pending_indexes {
        if let Some(schema) = schemas.iter_mut().find(|t| t.name == table_name) {
            schema.indexes.push(index);
        }
    }
    Ok(schemas)
}

/// Walks `sqlite_master` via `cursor` (root_page = 1) and returns every
/// `type IN ('table', 'view')` name, unfiltered and in storage order —
/// the shell's `.tables` needs both kinds of names but none of
/// [`TableSchema`]'s DDL parsing, so this bypasses [`read_schema`]
/// entirely rather than teaching it a schema-kind discriminator.
pub fn read_table_and_view_names<P: PageSource>(
    cursor: &mut TableCursor<P>,
    encoding: TextEncoding,
) -> Result<Vec<String>, DdlError> {
    let mut names = Vec::new();
    let mut row = cursor.first_row()?;
    while let Some(r) = row {
        let values = decode_record(&r.payload, encoding)?;
        if values.len() != 5 {
            return Err(DdlError::MalformedRow(values.len()));
        }
        if matches!(text(values.first()), "table" | "view") {
            names.push(text(values.get(1)).to_string());
        }
        row = cursor.next_row()?;
    }
    Ok(names)
}

/// A minimally-decoded `sqlite_master` view entry (#380): just the
/// name and verbatim `CREATE VIEW ...` source text.
/// `codegen::expand_views` re-parses `sql` into a `CreateView` AST
/// (query + optional explicit column list) when it needs to substitute
/// a `FROM`-clause reference to this view — kept as a plain string here
/// rather than an eagerly-parsed AST so this module (per the module doc)
/// stays independent of the full SQL parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSchema {
    /// The view's name.
    pub name: String,
    /// The verbatim `CREATE VIEW ...` source text.
    pub sql: String,
}

/// Walks `sqlite_master` via `cursor` (root_page = 1) and returns every
/// `type = 'view'` entry as a [`ViewSchema`] — the catalog counterpart
/// to [`read_schema`]'s tables, used by `codegen::expand_views` to
/// resolve a `FROM`-clause name against the view catalog before
/// falling back to a real table.
pub fn read_views<P: PageSource>(
    cursor: &mut TableCursor<P>,
    encoding: TextEncoding,
) -> Result<Vec<ViewSchema>, DdlError> {
    let mut views = Vec::new();
    let mut row = cursor.first_row()?;
    while let Some(r) = row {
        let values = decode_record(&r.payload, encoding)?;
        if values.len() != 5 {
            return Err(DdlError::MalformedRow(values.len()));
        }
        if text(values.first()) == "view" {
            views.push(ViewSchema {
                name: text(values.get(1)).to_string(),
                sql: text(values.get(4)).to_string(),
            });
        }
        row = cursor.next_row()?;
    }
    Ok(views)
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
            column_types: Vec::new(),
            column_collations: Vec::new(),
            is_virtual: true,
            sql: sql.to_string(),
            indexes: Vec::new(),
        };
    }

    let parsed = parse_create_table(sql).unwrap_or_default();
    TableSchema {
        name,
        root_page,
        columns: parsed.columns,
        without_rowid: parsed.without_rowid,
        strict: parsed.strict,
        column_types: parsed.column_types,
        column_collations: parsed.column_collations,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: Vec::new(),
    }
}

/// Parses a `sqlite_master` row with `type = 'index'` into the owning
/// table's name and a naive [`IndexSchema`]. Returns `None` for anything
/// this naive reader can't find a column list in — most notably
/// auto-indexes (`sql` is `NULL` in `sqlite_master`) — treated identically
/// to unparseable table DDL: graceful, never an error.
fn index_schema(values: &[Value]) -> Option<(String, IndexSchema)> {
    let name = text(values.get(1)).to_string();
    let table_name = text(values.get(2)).to_string();
    let root_page = match values.get(3) {
        Some(Value::Integer(i)) => *i as u32,
        _ => 0,
    };
    let sql = text(values.get(4));

    let (start, end) = column_list_span(sql)?;
    let inner = sql.get(start..end)?;
    let columns = split_top_level_commas(inner)
        .into_iter()
        .map(indexed_column)
        .collect();

    Some((
        table_name,
        IndexSchema {
            name,
            unique: is_unique_index(sql),
            columns,
            root_page,
        },
    ))
}

fn is_unique_index(sql: &str) -> bool {
    sql.trim_start()
        .to_ascii_uppercase()
        .starts_with("CREATE UNIQUE INDEX")
}

/// Splits a single indexed-column definition (`col`, `col DESC`, `col
/// COLLATE NOCASE ASC`, ...) into its column name and sort direction.
/// Expression indexes (`lower(col)`) are kept as raw text in `name` —
/// this reader doesn't parse expressions, matching the rest of this
/// module's "nothing more than the corpus needs" scope.
fn indexed_column(def: &str) -> IndexedColumn {
    let mut span = def.trim();
    let upper = span.to_ascii_uppercase();
    let mut desc = false;
    if let Some(rest) = upper.strip_suffix(" DESC") {
        desc = true;
        span = span[..rest.len()].trim_end();
    } else if let Some(rest) = upper.strip_suffix(" ASC") {
        span = span[..rest.len()].trim_end();
    }

    let collation = column_collation(span);

    let upper = span.to_ascii_uppercase();
    if let Some(pos) = upper.find(" COLLATE ") {
        span = span[..pos].trim_end();
    }

    IndexedColumn {
        name: span
            .trim_matches(['"', '`', '['].as_ref())
            .trim_matches([']'].as_ref())
            .to_string(),
        desc,
        collation,
    }
}

/// The [`Collation`] declared by a `COLLATE name` constraint anywhere in
/// `def` (a column definition or indexed-column spec), or
/// [`Collation::Binary`] when absent or unrecognized — matching SQLite's
/// own BINARY default.
fn column_collation(def: &str) -> Collation {
    let upper = def.to_ascii_uppercase();
    let Some(pos) = upper.find("COLLATE") else {
        return Collation::Binary;
    };
    let rest = def[pos.saturating_add("COLLATE".len())..].trim_start();
    let name = rest
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(['"', '`', '['].as_ref())
        .trim_matches([']'].as_ref());
    match name.to_ascii_uppercase().as_str() {
        "BINARY" => Collation::Binary,
        "NOCASE" => Collation::NoCase,
        "RTRIM" => Collation::RTrim,
        _ => Collation::Binary,
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
    column_types: Vec<String>,
    column_collations: Vec<Collation>,
    without_rowid: bool,
    strict: bool,
}

/// Parses `CREATE TABLE ... (col-defs) [table-options]`. Returns `None`
/// for anything this naive reader can't find a column list in — the
/// caller treats that identically to a virtual table (empty columns,
/// never an error).
fn parse_create_table(sql: &str) -> Option<ParsedCreateTable> {
    let (start, end) = column_list_span(sql)?;
    let inner = sql.get(start..end)?;
    let trailer = sql.get(end.saturating_add(1)..)?.to_ascii_uppercase();

    let defs: Vec<&str> = split_top_level_commas(inner)
        .into_iter()
        .filter(|def| !is_table_constraint(def))
        .collect();
    let columns = defs.iter().map(|def| column_name(def)).collect();
    let column_types = defs.iter().map(|def| column_type(def)).collect();
    let column_collations = defs.iter().map(|def| column_collation(def)).collect();

    Some(ParsedCreateTable {
        columns,
        column_types,
        column_collations,
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

/// Replaces the contents of quoted regions (`'...'`, `"..."`,
/// `` `...` ``, `[...]`) and comments (`--...`, `/*...*/`) with spaces,
/// preserving every other byte and the overall byte length — so a
/// masked offset always lines up with the same offset in the original
/// string. Callers that need to find top-level structure (parens,
/// commas, keywords) scan the masked bytes but slice the *original*
/// string for the text they return, so quoted content survives intact
/// while it can no longer be mistaken for syntax (#135: a comma or the
/// words PRIMARY/KEY inside a string literal used to be indistinguishable
/// from the real thing).
fn mask_quotes_and_comments(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0usize;
    let at = |i: usize| bytes.get(i).copied();
    let blank = |out: &mut [u8], i: usize| {
        if let Some(b) = out.get_mut(i) {
            *b = b' ';
        }
    };
    while let Some(here) = at(i) {
        match here {
            b'\'' | b'"' => {
                let quote = here;
                blank(&mut out, i);
                i = i.saturating_add(1);
                while let Some(b) = at(i) {
                    if b == quote {
                        blank(&mut out, i);
                        i = i.saturating_add(1);
                        // A doubled quote (`''`/`""`) is an escaped
                        // literal quote character, not the closing one.
                        if at(i) == Some(quote) {
                            blank(&mut out, i);
                            i = i.saturating_add(1);
                            continue;
                        }
                        break;
                    }
                    blank(&mut out, i);
                    i = i.saturating_add(1);
                }
            }
            b'`' | b'[' => {
                let close = if here == b'`' { b'`' } else { b']' };
                blank(&mut out, i);
                i = i.saturating_add(1);
                while let Some(b) = at(i) {
                    blank(&mut out, i);
                    i = i.saturating_add(1);
                    if b == close {
                        break;
                    }
                }
            }
            b'-' if at(i.saturating_add(1)) == Some(b'-') => {
                while let Some(b) = at(i) {
                    if b == b'\n' {
                        break;
                    }
                    blank(&mut out, i);
                    i = i.saturating_add(1);
                }
            }
            b'/' if at(i.saturating_add(1)) == Some(b'*') => {
                blank(&mut out, i);
                blank(&mut out, i.saturating_add(1));
                i = i.saturating_add(2);
                while let Some(b) = at(i) {
                    if b == b'*' && at(i.saturating_add(1)) == Some(b'/') {
                        blank(&mut out, i);
                        blank(&mut out, i.saturating_add(1));
                        i = i.saturating_add(2);
                        break;
                    }
                    blank(&mut out, i);
                    i = i.saturating_add(1);
                }
            }
            _ => i = i.saturating_add(1),
        }
    }
    out
}

/// Byte range *between* the outer parens of a `CREATE TABLE`'s
/// column-definition list, or `None` when there is no balanced list.
/// Paren depth is tracked over the quote/comment-masked text (#135) so a
/// paren inside a string literal can't unbalance the scan, but the
/// returned range indexes the original `sql` string.
fn column_list_span(sql: &str) -> Option<(usize, usize)> {
    let masked = mask_quotes_and_comments(sql);
    let start = masked.iter().position(|&b| b == b'(')?;
    let mut depth = 0i32;
    for (offset, &b) in masked.get(start..)?.iter().enumerate() {
        match b {
            b'(' => depth = depth.saturating_add(1),
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((start.saturating_add(1), start.saturating_add(offset)));
                }
            }
            _ => {}
        }
    }
    None
}

/// The single top-level-comma splitter behind both
/// [`parse_create_table`]'s `columns` and [`column_defs`]. These two
/// MUST agree position-for-position: [`rowid_alias_column`] returns an
/// index into `column_defs`, and `src/codegen/expr.rs` resolves that
/// index against `TableSchema::columns`. Two copies of this loop would
/// let the lists drift and silently mis-target the rowid substitution,
/// so they share one implementation rather than mirroring each other.
/// Splits over the quote/comment-masked text (#135) so a comma inside a
/// string literal (e.g. `DEFAULT 'a,b'`) is not mistaken for a column
/// separator; the returned slices still come from the original `inner`.
fn split_top_level_commas(inner: &str) -> Vec<&str> {
    let masked = mask_quotes_and_comments(inner);
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut part_start = 0usize;
    for (i, &b) in masked.iter().enumerate() {
        match b {
            b'(' => depth = depth.saturating_add(1),
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(inner.get(part_start..i).unwrap_or("").trim());
                part_start = i.saturating_add(1);
            }
            _ => {}
        }
    }
    parts.push(inner.get(part_start..).unwrap_or("").trim());
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

/// The declared-type text between a column's name and its first
/// column-constraint keyword — SQLite's own notion of "declared type"
/// (datatype3.html §3.1), which may span several words (`DOUBLE
/// PRECISION`, `UNSIGNED BIG INT`). Empty when the column has no type
/// (`CREATE TABLE t(x)`).
pub fn column_type(def: &str) -> String {
    const CONSTRAINT_KEYWORDS: [&str; 8] = [
        "PRIMARY",
        "NOT",
        "NULL",
        "UNIQUE",
        "CHECK",
        "DEFAULT",
        "COLLATE",
        "REFERENCES",
    ];
    def.split_whitespace()
        .skip(1)
        .take_while(|word| {
            let upper = word.to_ascii_uppercase();
            !CONSTRAINT_KEYWORDS
                .iter()
                .any(|kw| upper == *kw || upper.starts_with(&format!("{kw}(")))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits a `CREATE TABLE ...(col-defs)...` statement's column-definition
/// list into raw per-column definition strings, in declared order —
/// re-derived from `schema.sql` rather than kept alongside `columns`,
/// which holds names only — `src/dump.rs` needs each column's declared
/// type text, and [`rowid_alias_column`] needs its full constraint
/// text. Shares [`split_top_level_commas`] and [`is_table_constraint`]
/// with [`parse_create_table`], so the two column lists cannot drift.
pub fn column_defs(schema: &TableSchema) -> Vec<&str> {
    all_defs(schema)
        .into_iter()
        .filter(|def| !is_table_constraint(def))
        .collect()
}

/// Every top-level def in the column list, column definitions and
/// table-level constraints alike — the raw material [`column_defs`]
/// filters down and [`rowid_alias_column`] additionally needs the
/// constraint side of (to recognize a table-level `PRIMARY KEY(col)`).
fn all_defs(schema: &TableSchema) -> Vec<&str> {
    let Some((start, end)) = column_list_span(&schema.sql) else {
        return Vec::new();
    };
    let Some(inner) = schema.sql.get(start..end) else {
        return Vec::new();
    };
    split_top_level_commas(inner)
}

/// The table-level constraint defs `column_defs` filters out — the
/// counterpart `rowid_alias_column` scans for a `PRIMARY KEY(col)` form.
fn table_constraint_defs(schema: &TableSchema) -> Vec<&str> {
    all_defs(schema)
        .into_iter()
        .filter(|def| is_table_constraint(def))
        .collect()
}

/// Whether `def` is a column definition carrying an inline
/// `INTEGER PRIMARY KEY` (excluding the `DESC` form, which SQLite does
/// not treat as a rowid alias). Scans the quote/comment-masked text
/// (#135) so a string literal mentioning "primary key" — e.g.
/// `DEFAULT 'primary key'` — can't be mistaken for the real constraint.
fn is_integer_primary_key_inline(def: &str) -> bool {
    let masked = mask_quotes_and_comments(def);
    let masked = std::str::from_utf8(&masked).unwrap_or_default();
    let upper = masked.to_ascii_uppercase();
    let is_pk = upper
        .split(|c: char| !c.is_alphanumeric())
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| w == ["PRIMARY", "KEY"]);
    is_pk
        && upper.split_whitespace().any(|w| w == "INTEGER")
        // `INTEGER PRIMARY KEY DESC` is deliberately NOT a rowid
        // alias in SQLite — the DESC form gets its own b-tree index
        // and the column is stored normally, so substituting the
        // cursor's rowid would return values that aren't there.
        && !upper.split_whitespace().any(|w| w == "DESC")
}

/// Whether `def` declares an `INTEGER` type (masked, so a string literal
/// can't supply the word), regardless of any inline constraint.
fn is_integer_column(def: &str) -> bool {
    let masked = mask_quotes_and_comments(def);
    let masked = std::str::from_utf8(&masked).unwrap_or_default();
    masked
        .to_ascii_uppercase()
        .split_whitespace()
        .any(|w| w == "INTEGER")
}

/// If `constraint` is a table-level `PRIMARY KEY(col)` naming exactly
/// one column, returns that column's (unquoted) name.
fn primary_key_single_column(constraint: &str) -> Option<String> {
    let upper = constraint.trim_start().to_ascii_uppercase();
    if !upper.starts_with("PRIMARY KEY") {
        return None;
    }
    let open = constraint.find('(')?;
    let close = constraint.rfind(')')?;
    if close <= open {
        return None;
    }
    let inner = constraint.get(open.saturating_add(1)..close)?;
    let cols = split_top_level_commas(inner);
    match cols.as_slice() {
        [only] => Some(column_name(only)),
        _ => None,
    }
}

/// The one-column special case SQLite calls the rowid alias: a table
/// declared with a single `INTEGER PRIMARY KEY` column (not `WITHOUT
/// ROWID`) stores that column as a NULL placeholder in every record and
/// expects the reader to substitute the cursor's own rowid instead (see
/// `src/btree/mod.rs`'s module doc and spike 003 finding 1). Returns the
/// 0-based column index to substitute, if any.
///
/// Detection is textual and shares this module's documented naivety —
/// see the `known_fragile_*` tests below for the two forms it still
/// gets wrong. That matters more than it used to: `src/codegen/expr.rs`
/// now emits `Rowid` instead of `Column` based on this answer, so a
/// wrong index is a wrong query result, not just wrong `dump` output.
pub(crate) fn rowid_alias_column(schema: &TableSchema) -> Option<usize> {
    if schema.without_rowid {
        return None;
    }
    let defs = column_defs(schema);
    for (idx, def) in defs.iter().enumerate() {
        if is_integer_primary_key_inline(def) {
            return Some(idx);
        }
    }
    // The table-level `PRIMARY KEY(col)` form: SQLite only treats this
    // as a rowid alias when it names the table's one and only column,
    // and that column is INTEGER-typed (a composite key, or a second
    // column, rules it out).
    if let [only] = defs.as_slice() {
        if is_integer_column(only) {
            let col_name = column_name(only);
            let is_alias = table_constraint_defs(schema)
                .iter()
                .filter_map(|c| primary_key_single_column(c))
                .any(|pk_col| pk_col.eq_ignore_ascii_case(&col_name));
            if is_alias {
                return Some(0);
            }
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
            Value::Text("table".to_string().into()),
            Value::Text("weird".to_string().into()),
            Value::Text("weird".to_string().into()),
            Value::Integer(5),
            Value::Text("garbage with no parens".to_string().into()),
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
        // table t — read_schema must return only tables as top-level
        // entries, with idx_b attached to t's `indexes` (#211).
        let schemas = read_fixture("btrees", "index.db");
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "t");
        assert_eq!(schemas[0].indexes.len(), 1);
        assert_eq!(schemas[0].indexes[0].name, "idx_b");
        assert!(!schemas[0].indexes[0].unique);
        assert_eq!(schemas[0].indexes[0].columns.len(), 1);
        assert_eq!(schemas[0].indexes[0].columns[0].name, "b");
        assert!(!schemas[0].indexes[0].columns[0].desc);
    }

    #[test]
    fn unique_index_with_multiple_columns_and_desc() {
        let sql = "CREATE UNIQUE INDEX idx_ab ON t(a DESC, b)";
        let (table_name, index) = index_schema(&[
            Value::Null,
            Value::Text("idx_ab".to_string().into()),
            Value::Text("t".to_string().into()),
            Value::Integer(0),
            Value::Text(sql.to_string().into()),
        ])
        .expect("index_schema should parse a simple CREATE UNIQUE INDEX");
        assert_eq!(table_name, "t");
        assert!(index.unique);
        assert_eq!(index.columns.len(), 2);
        assert_eq!(index.columns[0].name, "a");
        assert!(index.columns[0].desc);
        assert_eq!(index.columns[1].name, "b");
        assert!(!index.columns[1].desc);
    }

    #[test]
    fn auto_index_with_null_sql_is_omitted() {
        // Auto-indexes created for PRIMARY KEY/UNIQUE constraints have a
        // NULL `sql` column in sqlite_master — this naive reader can't
        // find a column list in an empty string, so it's gracefully
        // skipped rather than erroring.
        let result = index_schema(&[
            Value::Null,
            Value::Text("sqlite_autoindex_t_1".to_string().into()),
            Value::Text("t".to_string().into()),
            Value::Integer(0),
            Value::Null,
        ]);
        assert!(result.is_none());
    }
}
