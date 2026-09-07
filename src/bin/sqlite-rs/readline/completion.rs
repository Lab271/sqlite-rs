// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Tab completion (#558): SQL keywords, dot-commands, and — when a
//! live schema is supplied — table/column names.

use sqlite_rs::schema::TableSchema;

/// Common SQL keywords worth completing. Not exhaustive (the parser's
/// own keyword table is private) — this is a completion candidate
/// list, not a source of truth for parsing.
const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "TABLE",
    "INDEX",
    "VIEW",
    "DROP",
    "ALTER",
    "ADD",
    "COLUMN",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "NOT",
    "NULL",
    "UNIQUE",
    "DEFAULT",
    "CHECK",
    "AND",
    "OR",
    "IN",
    "IS",
    "LIKE",
    "GLOB",
    "BETWEEN",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "ON",
    "AS",
    "DISTINCT",
    "UNION",
    "ALL",
    "EXCEPT",
    "INTERSECT",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "TRANSACTION",
    "PRAGMA",
    "EXPLAIN",
    "WITH",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
];

const DOT_COMMANDS: &[&str] = &[
    ".help",
    ".version",
    ".schema",
    ".dump",
    ".headers",
    ".mode",
    ".databases",
    ".indices",
    ".tables",
    ".quit",
    ".exit",
];

/// The word being completed: `line`'s chars in `start..cursor`, and
/// whether it starts a fresh word after a `.` (a dot-command context).
struct WordAtCursor {
    start: usize,
    prefix: String,
    is_dot_command: bool,
}

fn word_at_cursor(line: &str, cursor: usize) -> WordAtCursor {
    let chars: Vec<char> = line.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut start = cursor;
    while let Some(&c) = start.checked_sub(1).and_then(|i| chars.get(i)) {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            start = start.saturating_sub(1);
        } else {
            break;
        }
    }
    let prefix: String = chars
        .get(start..cursor)
        .map(|s| s.iter().collect())
        .unwrap_or_default();
    let is_dot_command = prefix.starts_with('.');
    WordAtCursor {
        start,
        prefix,
        is_dot_command,
    }
}

/// Returns `(replace_start, candidates)` — `replace_start` a char index
/// into `line` (matching the line editor's own char-indexed cursor) —
/// candidates that extend the word ending at `cursor` in `line`, given
/// the live `schemas` (may be empty, e.g. before a database is opened).
pub fn complete(line: &str, cursor: usize, schemas: &[TableSchema]) -> (usize, Vec<String>) {
    let word = word_at_cursor(line, cursor);

    if word.is_dot_command {
        let candidates = DOT_COMMANDS
            .iter()
            .filter(|c| c.starts_with(&word.prefix))
            .map(|c| c.to_string())
            .collect();
        return (word.start, candidates);
    }

    let upper_prefix = word.prefix.to_ascii_uppercase();
    let mut candidates: Vec<String> = KEYWORDS
        .iter()
        .filter(|k| k.starts_with(&upper_prefix))
        .map(|k| k.to_string())
        .collect();

    for schema in schemas {
        if schema.name.to_ascii_uppercase().starts_with(&upper_prefix) {
            candidates.push(schema.name.clone());
        }
        for col in &schema.columns {
            if col.to_ascii_uppercase().starts_with(&upper_prefix) {
                candidates.push(col.clone());
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    (word.start, candidates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlite_rs::record::Collation;

    fn schema(name: &str, columns: &[&str]) -> TableSchema {
        let columns: Vec<String> = columns.iter().map(|s| s.to_string()).collect();
        TableSchema {
            unresolved_autoindex: false,
            name: name.to_string(),
            root_page: 2,
            column_types: vec![String::new(); columns.len()],
            column_collations: vec![Collation::Binary; columns.len()],
            without_rowid: false,
            strict: false,
            is_virtual: false,
            sql: String::new(),
            indexes: Vec::new(),
            columns,
            rowid_alias: None,
        }
        .with_computed_rowid_alias()
    }

    #[test]
    fn completes_keyword_prefix() {
        let (start, cands) = complete("SEL", 3, &[]);
        assert_eq!(start, 0);
        assert!(cands.contains(&"SELECT".to_string()));
    }

    #[test]
    fn completes_dot_command() {
        let (start, cands) = complete(".tab", 4, &[]);
        assert_eq!(start, 0);
        assert_eq!(cands, vec![".tables".to_string()]);
    }

    #[test]
    fn completes_table_name_from_schema() {
        let schemas = vec![schema("widgets", &["id", "name"])];
        let (start, cands) = complete("SELECT * FROM wid", 17, &schemas);
        assert_eq!(start, 14);
        assert_eq!(cands, vec!["widgets".to_string()]);
    }

    #[test]
    fn completes_column_name_from_schema() {
        let schemas = vec![schema("widgets", &["identifier", "name"])];
        let (_, cands) = complete("SELECT ide", 10, &schemas);
        assert_eq!(cands, vec!["identifier".to_string()]);
    }

    #[test]
    fn no_word_yields_no_candidates_from_empty_prefix_but_all_keywords_would_match() {
        // Empty prefix after a space: every keyword "starts_with('')",
        // so this documents the (intentional) behavior rather than
        // asserting emptiness.
        let line = "SELECT * FROM t ";
        let (start, cands) = complete(line, line.len(), &[]);
        assert_eq!(start, line.len());
        assert!(cands.len() > 10);
    }
}
