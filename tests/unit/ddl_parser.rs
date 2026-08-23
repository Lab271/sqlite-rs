//! Unit tests for the V3 DDL parser: CREATE/DROP TABLE, CREATE/DROP INDEX
//! (issue #192, spec 002-parser).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use sqlite_rs::parser::ast::*;
use sqlite_rs::parser::{
    parse_create_index, parse_create_table, parse_create_view, parse_drop_index, parse_drop_table,
    parse_drop_view, ParseOutcome,
};

fn accept_view(src: &str) -> CreateView {
    match parse_create_view(src) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn accept_drop_view(src: &str) -> DropView {
    match parse_drop_view(src) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn accept_table(src: &str) -> CreateTable {
    match parse_create_table(src) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn unsupported_table(src: &str) -> String {
    match parse_create_table(src) {
        ParseOutcome::Unsupported { message, .. } => message,
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

fn invalid_table(src: &str) -> String {
    match parse_create_table(src) {
        ParseOutcome::Invalid { message, .. } => message,
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

fn accept_index(src: &str) -> CreateIndex {
    match parse_create_index(src) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn accept_drop_table(src: &str) -> DropTable {
    match parse_drop_table(src) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn accept_drop_index(src: &str) -> DropIndex {
    match parse_drop_index(src) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn unsupported_index(src: &str) -> String {
    match parse_create_index(src) {
        ParseOutcome::Unsupported { message, .. } => message,
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

fn invalid_index(src: &str) -> String {
    match parse_create_index(src) {
        ParseOutcome::Invalid { message, .. } => message,
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

fn unsupported_drop_table(src: &str) -> String {
    match parse_drop_table(src) {
        ParseOutcome::Unsupported { message, .. } => message,
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

fn invalid_drop_table(src: &str) -> String {
    match parse_drop_table(src) {
        ParseOutcome::Invalid { message, .. } => message,
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

fn unsupported_drop_index(src: &str) -> String {
    match parse_drop_index(src) {
        ParseOutcome::Unsupported { message, .. } => message,
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

fn invalid_drop_index(src: &str) -> String {
    match parse_drop_index(src) {
        ParseOutcome::Invalid { message, .. } => message,
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

// ---- CREATE TABLE ---------------------------------------------------------

#[test]
fn test_accept_create_table_basic() {
    let t = accept_table("CREATE TABLE t (a INTEGER, b TEXT)");
    assert!(!t.if_not_exists);
    assert_eq!(t.name, "t");
    assert_eq!(t.columns.len(), 2);
    assert_eq!(t.columns[0].name, "a");
    assert_eq!(t.columns[0].type_name.as_deref(), Some("INTEGER"));
    assert_eq!(t.columns[1].type_name.as_deref(), Some("TEXT"));
}

#[test]
fn test_accept_create_table_if_not_exists() {
    let t = accept_table("CREATE TABLE IF NOT EXISTS t (a)");
    assert!(t.if_not_exists);
}

#[test]
fn test_accept_create_table_no_type() {
    let t = accept_table("CREATE TABLE t (a, b)");
    assert_eq!(t.columns[0].type_name, None);
}

#[test]
fn test_accept_create_table_column_constraints() {
    let t = accept_table("CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT NOT NULL UNIQUE)");
    assert_eq!(
        t.columns[0].constraints,
        vec![ColumnConstraint::PrimaryKey {
            desc: None,
            autoincrement: false
        }]
    );
    assert_eq!(
        t.columns[1].constraints,
        vec![ColumnConstraint::NotNull, ColumnConstraint::Unique]
    );
}

#[test]
fn test_accept_create_table_primary_key_autoincrement() {
    let t = accept_table("CREATE TABLE t (a INTEGER PRIMARY KEY DESC AUTOINCREMENT)");
    assert_eq!(
        t.columns[0].constraints,
        vec![ColumnConstraint::PrimaryKey {
            desc: Some(true),
            autoincrement: true
        }]
    );
}

#[test]
fn test_accept_create_table_default_literal() {
    let t = accept_table("CREATE TABLE t (a INTEGER DEFAULT 5, b TEXT DEFAULT 'x')");
    let ColumnConstraint::Default(DefaultValue::Literal(expr)) = &t.columns[0].constraints[0]
    else {
        panic!("expected literal default");
    };
    assert_eq!(expr.kind, ExprKind::Literal(Literal::Integer(5)));
    let ColumnConstraint::Default(DefaultValue::Literal(expr)) = &t.columns[1].constraints[0]
    else {
        panic!("expected literal default");
    };
    assert_eq!(expr.kind, ExprKind::Literal(Literal::Str("x".to_string())));
}

#[test]
fn test_accept_create_table_default_negative_literal() {
    let t = accept_table("CREATE TABLE t (a INTEGER DEFAULT -1)");
    let ColumnConstraint::Default(DefaultValue::Literal(expr)) = &t.columns[0].constraints[0]
    else {
        panic!("expected literal default");
    };
    assert!(matches!(
        expr.kind,
        ExprKind::Unary {
            op: UnaryOp::Minus,
            ..
        }
    ));
}

#[test]
fn test_accept_create_table_default_paren_expr() {
    let t = accept_table("CREATE TABLE t (a INTEGER DEFAULT (1 + 2))");
    assert!(matches!(
        &t.columns[0].constraints[0],
        ColumnConstraint::Default(DefaultValue::Paren(_))
    ));
}

#[test]
fn test_accept_create_table_check_column_constraint() {
    let t = accept_table("CREATE TABLE t (a INTEGER CHECK (a > 0))");
    assert!(matches!(
        &t.columns[0].constraints[0],
        ColumnConstraint::Check(_)
    ));
}

#[test]
fn test_accept_create_table_collate_column_constraint() {
    let t = accept_table("CREATE TABLE t (a TEXT COLLATE NOCASE)");
    assert_eq!(
        t.columns[0].constraints,
        vec![ColumnConstraint::Collate("NOCASE".to_string())]
    );
}

#[test]
fn test_accept_create_table_named_constraint() {
    let t = accept_table("CREATE TABLE t (a INTEGER CONSTRAINT pk PRIMARY KEY)");
    assert_eq!(
        t.columns[0].constraints,
        vec![ColumnConstraint::PrimaryKey {
            desc: None,
            autoincrement: false
        }]
    );
}

#[test]
fn test_accept_create_table_constraint() {
    let t = accept_table("CREATE TABLE t (a, b, PRIMARY KEY (a, b))");
    assert_eq!(t.constraints.len(), 1);
    let TableConstraint::PrimaryKey(cols) = &t.constraints[0] else {
        panic!("expected PrimaryKey constraint");
    };
    assert_eq!(cols.len(), 2);
}

#[test]
fn test_accept_create_table_unique_constraint_with_collate_desc() {
    let t = accept_table("CREATE TABLE t (a, b, UNIQUE (a COLLATE NOCASE DESC))");
    let TableConstraint::Unique(cols) = &t.constraints[0] else {
        panic!("expected Unique constraint");
    };
    assert_eq!(cols[0].desc, Some(true));
    assert!(matches!(cols[0].expr.kind, ExprKind::Collate { .. }));
}

#[test]
fn test_accept_create_table_check_constraint() {
    let t = accept_table("CREATE TABLE t (a, CHECK (a > 0))");
    assert!(matches!(t.constraints[0], TableConstraint::Check(_)));
}

#[test]
fn test_accept_create_table_without_rowid() {
    let t = accept_table("CREATE TABLE t (a PRIMARY KEY) WITHOUT ROWID");
    assert!(t.without_rowid);
    assert!(!t.strict);
}

#[test]
fn test_accept_create_table_strict() {
    let t = accept_table("CREATE TABLE t (a INT) STRICT");
    assert!(t.strict);
    assert!(!t.without_rowid);
}

#[test]
fn test_unsupported_create_table_references() {
    unsupported_table("CREATE TABLE t (a INTEGER REFERENCES other(id))");
}

#[test]
fn test_unsupported_create_table_foreign_key() {
    unsupported_table("CREATE TABLE t (a, FOREIGN KEY (a) REFERENCES other(id))");
}

#[test]
fn test_unsupported_create_virtual_table() {
    unsupported_table("CREATE VIRTUAL TABLE t USING fts5(a, b)");
}

#[test]
fn test_unsupported_create_temp_table() {
    unsupported_table("CREATE TEMP TABLE t (a)");
}

#[test]
fn test_unsupported_create_table_on_conflict() {
    unsupported_table("CREATE TABLE t (a UNIQUE ON CONFLICT FAIL)");
}

#[test]
fn test_invalid_create_table_missing_paren() {
    invalid_table("CREATE TABLE t a INTEGER)");
}

#[test]
fn test_invalid_create_table_dangling_column_constraint_name() {
    invalid_table("CREATE TABLE t (a INTEGER CONSTRAINT foo)");
}

#[test]
fn test_printer_roundtrip_create_table() {
    let t = accept_table(
        "CREATE TABLE IF NOT EXISTS t (a INTEGER PRIMARY KEY, b TEXT NOT NULL DEFAULT 'x', CHECK (a > 0)) STRICT",
    );
    let printed = t.to_string();
    let reparsed = accept_table(&printed);
    assert_eq!(t, reparsed, "printed: {printed}");
}

#[test]
fn test_printer_roundtrip_create_table_without_rowid() {
    let t = accept_table("CREATE TABLE t (a INTEGER, PRIMARY KEY (a)) WITHOUT ROWID");
    let printed = t.to_string();
    let reparsed = accept_table(&printed);
    assert_eq!(t, reparsed, "printed: {printed}");
}

// ---- CREATE INDEX ----------------------------------------------------------

#[test]
fn test_accept_create_index_basic() {
    let idx = accept_index("CREATE INDEX i ON t (a)");
    assert!(!idx.unique);
    assert!(!idx.if_not_exists);
    assert_eq!(idx.name, "i");
    assert_eq!(idx.table, "t");
    assert_eq!(idx.columns.len(), 1);
}

#[test]
fn test_accept_create_unique_index_if_not_exists() {
    let idx = accept_index("CREATE UNIQUE INDEX IF NOT EXISTS i ON t (a, b DESC)");
    assert!(idx.unique);
    assert!(idx.if_not_exists);
    assert_eq!(idx.columns.len(), 2);
    assert_eq!(idx.columns[1].desc, Some(true));
}

#[test]
fn test_accept_create_index_partial() {
    let idx = accept_index("CREATE INDEX i ON t (a) WHERE a > 0");
    assert!(idx.where_clause.is_some());
}

#[test]
fn test_accept_create_index_collate() {
    let idx = accept_index("CREATE INDEX i ON t (a COLLATE NOCASE)");
    assert!(matches!(idx.columns[0].expr.kind, ExprKind::Collate { .. }));
}

#[test]
fn test_printer_roundtrip_create_index() {
    let idx = accept_index("CREATE UNIQUE INDEX IF NOT EXISTS i ON t (a, b DESC) WHERE a > 0");
    let printed = idx.to_string();
    let reparsed = accept_index(&printed);
    assert_eq!(idx, reparsed, "printed: {printed}");
}

// ---- DROP TABLE / DROP INDEX ------------------------------------------------

#[test]
fn test_accept_drop_table() {
    let d = accept_drop_table("DROP TABLE t");
    assert!(!d.if_exists);
    assert_eq!(d.name, "t");
}

#[test]
fn test_accept_drop_table_if_exists() {
    let d = accept_drop_table("DROP TABLE IF EXISTS t");
    assert!(d.if_exists);
}

#[test]
fn test_accept_drop_index() {
    let d = accept_drop_index("DROP INDEX i");
    assert!(!d.if_exists);
    assert_eq!(d.name, "i");
}

#[test]
fn test_accept_drop_index_if_exists() {
    let d = accept_drop_index("DROP INDEX IF EXISTS i");
    assert!(d.if_exists);
}

#[test]
fn test_printer_roundtrip_drop_table() {
    let d = accept_drop_table("DROP TABLE IF EXISTS t");
    let printed = d.to_string();
    let reparsed = accept_drop_table(&printed);
    assert_eq!(d, reparsed, "printed: {printed}");
}

#[test]
fn test_printer_roundtrip_drop_index() {
    let d = accept_drop_index("DROP INDEX IF EXISTS i");
    let printed = d.to_string();
    let reparsed = accept_drop_index(&printed);
    assert_eq!(d, reparsed, "printed: {printed}");
}

// ---- issue #224: expect_end branches (trailing tokens after a valid parse) -

#[test]
fn test_unsupported_create_table_trailing_compound() {
    unsupported_table("CREATE TABLE t (a INTEGER) UNION SELECT 1");
}

#[test]
fn test_invalid_create_table_trailing_garbage() {
    invalid_table("CREATE TABLE t (a INTEGER) EXTRA");
}

#[test]
fn test_unsupported_create_index_trailing_compound() {
    unsupported_index("CREATE INDEX i ON t (a) UNION SELECT 1");
}

#[test]
fn test_invalid_create_index_trailing_garbage() {
    invalid_index("CREATE INDEX i ON t (a) EXTRA");
}

#[test]
fn test_invalid_create_index_missing_on() {
    invalid_index("CREATE INDEX i t (a)");
}

#[test]
fn test_unsupported_drop_table_trailing_compound() {
    unsupported_drop_table("DROP TABLE t UNION SELECT 1");
}

#[test]
fn test_invalid_drop_table_trailing_garbage() {
    invalid_drop_table("DROP TABLE t EXTRA");
}

#[test]
fn test_invalid_drop_table_missing_name() {
    invalid_drop_table("DROP TABLE");
}

#[test]
fn test_unsupported_drop_index_trailing_compound() {
    unsupported_drop_index("DROP INDEX i UNION SELECT 1");
}

#[test]
fn test_invalid_drop_index_trailing_garbage() {
    invalid_drop_index("DROP INDEX i EXTRA");
}

#[test]
fn test_invalid_drop_index_missing_name() {
    invalid_drop_index("DROP INDEX");
}

#[test]
fn test_accept_create_view_simple() {
    let view = accept_view("CREATE VIEW v AS SELECT a, b FROM t");
    assert_eq!(view.name, "v");
    assert!(view.columns.is_none());
    assert!(!view.if_not_exists);
    assert_eq!(view.query.columns.len(), 2);
}

#[test]
fn test_accept_create_view_with_column_list() {
    let view = accept_view("CREATE VIEW v (x, y) AS SELECT a, b FROM t");
    assert_eq!(view.name, "v");
    assert_eq!(view.columns, Some(vec!["x".to_string(), "y".to_string()]));
}

#[test]
fn test_accept_create_view_if_not_exists() {
    let view = accept_view("CREATE VIEW IF NOT EXISTS v AS SELECT 1");
    assert!(view.if_not_exists);
}

#[test]
fn test_printer_roundtrip_create_view() {
    let view = accept_view("CREATE VIEW v (x, y) AS SELECT a, b FROM t");
    let printed = view.to_string();
    let reparsed = accept_view(&printed);
    assert_eq!(reparsed.name, view.name);
    assert_eq!(reparsed.columns, view.columns);
}

#[test]
fn test_accept_drop_view() {
    let drop = accept_drop_view("DROP VIEW v");
    assert_eq!(drop.name, "v");
    assert!(!drop.if_exists);
}

#[test]
fn test_accept_drop_view_if_exists() {
    let drop = accept_drop_view("DROP VIEW IF EXISTS v");
    assert!(drop.if_exists);
}
