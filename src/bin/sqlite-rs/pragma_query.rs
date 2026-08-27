// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Read-only, result-set-producing introspection `PRAGMA`s (#489):
//! `table_info`, `table_list`, `index_list`, `index_info`,
//! `database_list`, `schema_version`, `user_version`, `page_size`,
//! `page_count`.
//!
//! Deliberately kept OUT of the main SQL grammar/AST/codegen/VDBE
//! pipeline — unlike `journal_mode` (#388, `src/parser/grammar.rs`'s
//! `parse_pragma_stmt` -> `src/codegen/pragma.rs` -> a `SetJournalMode`
//! VDBE opcode), these never write anything and have no bytecode to
//! compile: they're synthetic in-memory result sets built directly from
//! already-loaded schema/header data, exactly like `EXPLAIN QUERY
//! PLAN`'s `SelectOutcome::Eqp` (`query.rs`). A hand-rolled recognizer
//! here (`PRAGMA <ident>[(<ident-or-string>)]`) is all that's needed —
//! no dependency on the real tokenizer/parser.
//!
//! Shared verbatim by the `query` subcommand (`query.rs`) and `repl.rs`
//! so the two CLI surfaces can't drift on which 9 names are recognized
//! or how their rows are shaped.

use std::path::Path;

use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::schema::{column_defs, column_type, TableSchema, ViewSchema};

/// One of the 9 recognized read-only introspection pragmas, already
/// parsed out of its argument (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PragmaQuery {
    TableInfo(String),
    TableList,
    IndexList(String),
    IndexInfo(String),
    DatabaseList,
    SchemaVersion,
    UserVersion,
    PageSize,
    PageCount,
}

/// Recognizes `PRAGMA <name>[(<arg>)] [;]`, case-insensitive on
/// `PRAGMA` and `<name>`. Returns `None` both for non-`PRAGMA`
/// statements and for a `PRAGMA` whose name isn't one of the 9 handled
/// here (e.g. `journal_mode`) — callers fall through to their existing
/// behavior in that case, so this never shadows the write-pragma path.
pub(crate) fn parse_pragma_query(sql: &str) -> Option<PragmaQuery> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let rest = strip_ci_prefix(trimmed, "pragma")?;
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }

    // Split `<name>` from an optional `(<arg>)`, allowing whitespace
    // around the parens (`PRAGMA table_info (t)` is accepted by real
    // `sqlite3` too).
    let (name, arg) = match rest.find('(') {
        Some(open) => {
            let name = rest.get(..open)?.trim();
            let after_open = rest.get(open.saturating_add(1)..)?;
            let close_rel = after_open.find(')')?;
            let inner = after_open.get(..close_rel)?.trim();
            (name, Some(unquote_ident(inner)))
        }
        None => (rest.trim(), None),
    };

    match (name.to_ascii_lowercase().as_str(), arg) {
        ("table_info", Some(t)) => Some(PragmaQuery::TableInfo(t)),
        ("table_list", None) => Some(PragmaQuery::TableList),
        ("index_list", Some(t)) => Some(PragmaQuery::IndexList(t)),
        ("index_info", Some(i)) => Some(PragmaQuery::IndexInfo(i)),
        ("database_list", None) => Some(PragmaQuery::DatabaseList),
        ("schema_version", None) => Some(PragmaQuery::SchemaVersion),
        ("user_version", None) => Some(PragmaQuery::UserVersion),
        ("page_size", None) => Some(PragmaQuery::PageSize),
        ("page_count", None) => Some(PragmaQuery::PageCount),
        _ => None,
    }
}

/// Case-insensitive `str::strip_prefix`, restricted to a whole leading
/// word (so `pragmatic` isn't mistaken for `pragma` followed by `tic`).
fn strip_ci_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    match s.as_bytes().get(prefix.len()) {
        None => Some(""),
        Some(b) if b.is_ascii_whitespace() => Some(&s[prefix.len()..]),
        _ => None,
    }
}

/// Strips a single layer of matching `'...'`, `"..."`, or `` `...` ``
/// quoting from a pragma argument (`PRAGMA table_info('t')` and
/// `PRAGMA table_info(t)` name the same table).
fn unquote_ident(s: &str) -> String {
    let bytes = s.as_bytes();
    if let (Some(&first), Some(&last)) = (bytes.first(), bytes.last()) {
        if bytes.len() >= 2 && (first == b'\'' || first == b'"' || first == b'`') && first == last {
            if let Some(inner) = s.get(1..s.len().saturating_sub(1)) {
                return inner.to_string();
            }
        }
    }
    s.to_string()
}

/// Executes an already-parsed [`PragmaQuery`] against loaded
/// schema/header data, returning rows of already-`to_string`-ed column
/// values ready for pipe-delimited printing — the same shape
/// `SelectOutcome::Eqp` rows and ordinary query rows are rendered in.
pub(crate) fn execute_pragma_query(
    query: &PragmaQuery,
    schemas: &[TableSchema],
    views: &[ViewSchema],
    header: &DatabaseHeader,
    db_path: &Path,
) -> Result<Vec<Vec<String>>, String> {
    match query {
        PragmaQuery::TableInfo(name) => table_info(schemas, name),
        PragmaQuery::TableList => Ok(table_list(schemas, views)),
        PragmaQuery::IndexList(name) => index_list(schemas, name),
        PragmaQuery::IndexInfo(name) => index_info(schemas, name),
        PragmaQuery::DatabaseList => Ok(vec![database_list(db_path)]),
        PragmaQuery::SchemaVersion => Ok(vec![vec![header.schema_cookie.to_string()]]),
        PragmaQuery::UserVersion => Ok(vec![vec![header.user_version.to_string()]]),
        PragmaQuery::PageSize => Ok(vec![vec![header.page_size.to_string()]]),
        PragmaQuery::PageCount => Ok(vec![vec![header.page_count.to_string()]]),
    }
}

fn find_table<'a>(schemas: &'a [TableSchema], name: &str) -> Result<&'a TableSchema, String> {
    schemas
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no such table: {name}"))
}

/// `PRAGMA table_info(name)`: `cid|name|type|notnull|dflt_value|pk`,
/// one row per column, matching stock `sqlite3`'s column order and
/// semantics (verified against a real `sqlite3` — see `pragma_query.rs`
/// tests / `tests/unit/introspection_pragmas.rs`). `pk` is 0 for a
/// non-key column, otherwise its 1-based position within the table's
/// primary key (matching a `WITHOUT ROWID` composite key, where it's
/// not just a boolean).
fn table_info(schemas: &[TableSchema], name: &str) -> Result<Vec<Vec<String>>, String> {
    let schema = find_table(schemas, name)?;
    let defs = column_defs(schema);
    let pk_columns = primary_key_columns(schema);
    let mut rows = Vec::with_capacity(schema.columns.len());
    for (cid, col_name) in schema.columns.iter().enumerate() {
        let def = defs.get(cid).copied().unwrap_or("");
        let declared_type = column_type(def);
        let in_pk = pk_columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(col_name));
        // A `WITHOUT ROWID` table's primary-key columns are implicitly
        // `NOT NULL` even with no explicit constraint written — stock
        // `sqlite3` reports `notnull=1` for them regardless. An ordinary
        // rowid table's `INTEGER PRIMARY KEY` alias column has no such
        // implicit rule (`notnull=0` unless declared).
        let notnull = column_notnull(def) || (schema.without_rowid && in_pk.is_some());
        let dflt_value = column_default(def).unwrap_or_default();
        let pk = in_pk.map(|i| i.saturating_add(1)).unwrap_or(0);
        rows.push(vec![
            cid.to_string(),
            col_name.clone(),
            declared_type,
            u8::from(notnull).to_string(),
            dflt_value,
            pk.to_string(),
        ]);
    }
    Ok(rows)
}

/// `PRAGMA table_list`: `schema|name|type|ncol|wr|strict`, one row per
/// table/view. Single-database scope (no `ATTACH`, no temp-db support)
/// — `schema` is always `main`; stock `sqlite3` additionally lists
/// `sqlite_schema`/`sqlite_temp_schema` internal rows, deliberately
/// omitted here since this reader has no notion of them as a queryable
/// table.
fn table_list(schemas: &[TableSchema], views: &[ViewSchema]) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = schemas
        .iter()
        .map(|s| {
            vec![
                "main".to_string(),
                s.name.clone(),
                "table".to_string(),
                s.columns.len().to_string(),
                u8::from(s.without_rowid).to_string(),
                u8::from(s.strict).to_string(),
            ]
        })
        .collect();
    rows.extend(views.iter().map(|v| {
        vec![
            "main".to_string(),
            v.name.clone(),
            "view".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
        ]
    }));
    rows
}

/// `PRAGMA index_list(name)`: `seq|name|unique|origin|partial`, one row
/// per index on table `name`. Scope cut: only explicit `CREATE
/// INDEX`/`CREATE UNIQUE INDEX` entries are reported (`origin` is
/// always `c`) — auto-indexes SQLite creates implicitly for an inline
/// `PRIMARY KEY`/`UNIQUE` column constraint (`origin` `pk`/`u`) have a
/// `NULL` `sqlite_master.sql` and are already dropped by
/// `schema::read_schema` (see `TableSchema::indexes`' doc comment), not
/// something this pragma path re-derives. `partial` is always `0` — no
/// partial-index (`WHERE`) tracking in `IndexSchema`.
fn index_list(schemas: &[TableSchema], table: &str) -> Result<Vec<Vec<String>>, String> {
    let schema = find_table(schemas, table)?;
    Ok(schema
        .indexes
        .iter()
        .enumerate()
        .map(|(seq, idx)| {
            vec![
                seq.to_string(),
                idx.name.clone(),
                u8::from(idx.unique).to_string(),
                "c".to_string(),
                "0".to_string(),
            ]
        })
        .collect())
}

/// `PRAGMA index_info(name)`: `seqno|cid|name`, one row per column in
/// the named index, `cid` resolved against the owning table's column
/// list. Errors if no table's `indexes` carries this index name.
fn index_info(schemas: &[TableSchema], index_name: &str) -> Result<Vec<Vec<String>>, String> {
    for schema in schemas {
        let Some(index) = schema
            .indexes
            .iter()
            .find(|i| i.name.eq_ignore_ascii_case(index_name))
        else {
            continue;
        };
        return Ok(index
            .columns
            .iter()
            .enumerate()
            .map(|(seqno, col)| {
                let cid = schema
                    .columns
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(&col.name))
                    .map(|i| i as i64)
                    .unwrap_or(-1);
                vec![seqno.to_string(), cid.to_string(), col.name.clone()]
            })
            .collect());
    }
    Err(format!("no such index: {index_name}"))
}

/// `PRAGMA database_list`: one row, `0|main|<absolute path>` — no
/// `ATTACH` support, single-file only.
fn database_list(db_path: &Path) -> Vec<String> {
    let absolute = std::fs::canonicalize(db_path)
        .unwrap_or_else(|_| db_path.to_path_buf())
        .display()
        .to_string();
    vec!["0".to_string(), "main".to_string(), absolute]
}

/// Whether `def` (one column definition, from [`column_defs`]) carries
/// an explicit `NOT NULL` constraint. Word-boundary scan, naive in the
/// same sense as the rest of `schema::ddl_reader` — no quote-masking,
/// so a string literal spelling out "not null" could false-positive
/// (accepted scope cut, consistent with that module's own documented
/// naivety).
fn column_notnull(def: &str) -> bool {
    let words: Vec<String> = def
        .split_whitespace()
        .map(|w| w.to_ascii_uppercase())
        .collect();
    words
        .windows(2)
        .any(|w| matches!(w, [a, b] if a == "NOT" && b == "NULL"))
}

/// The column-level `DEFAULT` expression text for `def`, if any — kept
/// verbatim (including any quoting) as stock `sqlite3` echoes it.
/// Handles a parenthesized expression default (`DEFAULT (expr)`) by
/// paren-depth balancing; otherwise takes the single next token
/// (string literal, number, or bare keyword like `CURRENT_TIMESTAMP`).
/// A multi-word quoted string default (`DEFAULT 'a b'`) is reassembled
/// by continuing to collect words until the closing quote is seen.
fn column_default(def: &str) -> Option<String> {
    let words: Vec<&str> = def.split_whitespace().collect();
    let pos = words
        .iter()
        .position(|w| w.eq_ignore_ascii_case("DEFAULT"))?;
    let rest = words.get(pos.saturating_add(1)..)?;
    let first = *rest.first()?;

    if first.starts_with('(') {
        let mut depth = 0i32;
        let mut collected = Vec::new();
        for w in rest {
            collected.push(*w);
            let opens = i32::try_from(w.matches('(').count()).unwrap_or(i32::MAX);
            let closes = i32::try_from(w.matches(')').count()).unwrap_or(i32::MAX);
            depth = depth.saturating_add(opens).saturating_sub(closes);
            if depth <= 0 {
                break;
            }
        }
        return Some(collected.join(" "));
    }

    if first.starts_with('\'') {
        let mut collected = vec![first];
        let mut closed = first.len() > 1 && first.ends_with('\'');
        for w in rest.iter().skip(1) {
            if closed {
                break;
            }
            collected.push(*w);
            if w.ends_with('\'') {
                closed = true;
            }
        }
        return Some(collected.join(" "));
    }

    Some(first.trim_end_matches(',').to_string())
}

/// The (unquoted) leading identifier of a column/constraint definition
/// or an indexed-column entry — first whitespace-delimited token, minus
/// surrounding quote/bracket punctuation.
fn column_name(def: &str) -> String {
    def.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(['"', '`', '['])
        .trim_matches([']'])
        .to_string()
}

/// Whether `def` (a single column definition) carries an inline
/// `PRIMARY KEY` constraint (any type — unlike
/// `schema::ddl_reader::rowid_alias_column`'s `INTEGER`-only check,
/// `table_info`'s `pk` column applies regardless of declared type,
/// e.g. `TEXT PRIMARY KEY`). No quote-masking (naive, see
/// `column_notnull`'s doc comment for the same accepted tradeoff).
fn is_primary_key_inline(def: &str) -> bool {
    let upper: Vec<String> = def
        .split_whitespace()
        .map(|w| w.to_ascii_uppercase())
        .collect();
    upper
        .windows(2)
        .any(|w| matches!(w, [a, b] if a == "PRIMARY" && b == "KEY"))
}

/// The table's primary-key column names, in declared key order — a
/// single inline `col ... PRIMARY KEY` column, or a table-level
/// `PRIMARY KEY(col1, col2, ...)` constraint, or empty if the table has
/// neither (e.g. a rowid table with no declared key at all).
fn primary_key_columns(schema: &TableSchema) -> Vec<String> {
    let defs = column_defs(schema);
    for (idx, def) in defs.iter().enumerate() {
        if is_primary_key_inline(def) {
            return vec![schema.columns.get(idx).cloned().unwrap_or_default()];
        }
    }

    // No inline PK column: look for a table-level `PRIMARY KEY(...)`
    // constraint in the raw DDL text. Naive scan (no quote-masking,
    // no disambiguation from a same-named table-level `UNIQUE`/`CHECK`
    // constraint appearing first) — same accepted tradeoff as the rest
    // of this module; the corpus/tests this pragma targets don't
    // exercise those edge cases.
    let upper = schema.sql.to_ascii_uppercase();
    let Some(kw) = upper.find("PRIMARY KEY") else {
        return vec![];
    };
    let Some(after) = schema.sql.get(kw.saturating_add("PRIMARY KEY".len())..) else {
        return vec![];
    };
    let Some(open_rel) = after.find('(') else {
        return vec![];
    };
    let Some(after_open) = after.get(open_rel.saturating_add(1)..) else {
        return vec![];
    };
    let Some(close_rel) = after_open.find(')') else {
        return vec![];
    };
    after_open
        .get(..close_rel)
        .unwrap_or("")
        .split(',')
        .map(|c| column_name(c.trim()))
        .filter(|c| !c.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pragma_with_and_without_parens_and_quoting() {
        assert_eq!(
            parse_pragma_query("PRAGMA table_info(t)"),
            Some(PragmaQuery::TableInfo("t".to_string()))
        );
        assert_eq!(
            parse_pragma_query("pragma TABLE_INFO('t');"),
            Some(PragmaQuery::TableInfo("t".to_string()))
        );
        assert_eq!(
            parse_pragma_query("  PRAGMA page_count ; "),
            Some(PragmaQuery::PageCount)
        );
        assert_eq!(parse_pragma_query("PRAGMA journal_mode"), None);
        assert_eq!(parse_pragma_query("SELECT 1"), None);
        assert_eq!(parse_pragma_query("PRAGMATIC(1)"), None);
    }

    #[test]
    fn column_default_handles_string_number_and_expr() {
        assert_eq!(
            column_default("b TEXT NOT NULL DEFAULT 'x'"),
            Some("'x'".to_string())
        );
        assert_eq!(column_default("n INTEGER DEFAULT 5"), Some("5".to_string()));
        assert_eq!(column_default("n INTEGER"), None);
        assert_eq!(
            column_default("n INTEGER DEFAULT (1 + 2)"),
            Some("(1 + 2)".to_string())
        );
    }

    #[test]
    fn primary_key_columns_inline_and_table_level() {
        let mut schema = TableSchema {
            name: "t".to_string(),
            root_page: 2,
            columns: vec!["a".to_string(), "b".to_string()],
            column_types: vec!["INTEGER".to_string(), "TEXT".to_string()],
            column_collations: vec![],
            without_rowid: false,
            strict: false,
            is_virtual: false,
            sql: "CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT)".to_string(),
            indexes: vec![],
        };
        assert_eq!(primary_key_columns(&schema), vec!["a".to_string()]);

        schema.sql = "CREATE TABLE t (a INTEGER, b TEXT, PRIMARY KEY (a, b))".to_string();
        schema.without_rowid = true;
        assert_eq!(
            primary_key_columns(&schema),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}
