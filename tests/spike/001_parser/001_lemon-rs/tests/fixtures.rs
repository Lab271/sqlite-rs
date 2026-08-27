// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Fixture-driven acceptance tests for the shared spike corpus.
//!
//! Every statement in `../fixtures/valid.sql` must parse; every statement in
//! `../fixtures/invalid.sql` must produce an error (never a panic).

use spike_lemon_rs::parse;

const VALID: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/valid.sql"));
const INVALID: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../fixtures/invalid.sql"
));

/// Statements are separated by `;`. The fixtures contain no semicolons inside
/// string literals apart from the deliberately unterminated one in
/// invalid.sql, which stays attached to its (still failing) statement.
pub fn statements(text: &str) -> Vec<&str> {
    text.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn all_valid_statements_parse() {
    let stmts = statements(VALID);
    assert_eq!(stmts.len(), 30, "unexpected fixture count in valid.sql");

    let mut failures = Vec::new();
    for sql in &stmts {
        match parse(sql) {
            Ok(_) => {}
            Err(e) => failures.push(format!("{sql}  =>  {e}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} valid statements failed:\n{}",
        failures.len(),
        stmts.len(),
        failures.join("\n")
    );
}

#[test]
fn all_invalid_statements_fail() {
    let stmts = statements(INVALID);
    assert_eq!(stmts.len(), 20, "unexpected fixture count in invalid.sql");

    let mut accepted = Vec::new();
    for sql in &stmts {
        if let Ok(ast) = parse(sql) {
            accepted.push(format!("{sql}  =>  {ast:?}"));
        }
    }
    assert!(
        accepted.is_empty(),
        "{} of {} invalid statements were accepted:\n{}",
        accepted.len(),
        stmts.len(),
        accepted.join("\n")
    );
}

#[test]
fn semantic_actions_build_a_tree() {
    use spike_lemon_rs::ast::*;

    // Precedence ladder: 1 + 2 * 3 = 7 must group as (1 + (2 * 3)) = 7.
    let Ok(Stmt::Select(select)) = parse("SELECT a FROM t WHERE 1 + 2 * 3 = 7") else {
        panic!("expected a SELECT");
    };
    let Some(Expr::Binary { op, lhs, rhs }) = select.where_clause else {
        panic!("expected a binary WHERE expression");
    };
    assert_eq!(op, BinOp::Eq);
    assert_eq!(*rhs, Expr::Lit(Lit::Int(7)));
    let Expr::Binary { op, rhs, .. } = *lhs else {
        panic!("expected 1 + (2 * 3)");
    };
    assert_eq!(op, BinOp::Add);
    assert!(matches!(*rhs, Expr::Binary { op: BinOp::Mul, .. }));

    // CREATE TABLE column constraints.
    let Ok(Stmt::CreateTable {
        if_not_exists,
        name,
        columns,
    }) = parse("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
    else {
        panic!("expected a CREATE TABLE");
    };
    assert!(if_not_exists);
    assert_eq!(name, "users");
    assert_eq!(
        columns,
        vec![
            ColumnDef {
                name: "id".into(),
                type_name: Some("INTEGER".into()),
                not_null: false,
                primary_key: true
            },
            ColumnDef {
                name: "name".into(),
                type_name: Some("TEXT".into()),
                not_null: true,
                primary_key: false
            },
        ]
    );

    // Aggregate with DISTINCT, and a qualified column reference.
    let Ok(Stmt::Select(select)) = parse("SELECT count(DISTINCT t.a) FROM t") else {
        panic!("expected a SELECT");
    };
    let ResultColumn::Expr(Expr::Func { name, distinct, args }, None) = &select.columns[0] else {
        panic!("expected a function call");
    };
    assert_eq!(name, "count");
    assert!(distinct);
    assert_eq!(
        args,
        &vec![Expr::Column {
            table: Some("t".into()),
            name: "a".into()
        }]
    );
}

#[test]
fn degenerate_input_never_panics() {
    // Empty input, lone keywords, junk characters.
    for sql in ["", "   ", "SELECT", "$$$", "'", "\"", "1 + 1"] {
        assert!(parse(sql).is_err(), "expected {sql:?} to be rejected");
    }
    // FROM is optional in the subset grammar, so this one is legitimately valid.
    assert!(parse("SELECT *").is_ok());
    // Deep nesting: the parser stack starts at YYSTACKDEPTH (50) entries and has
    // to grow well past that.
    let deep = format!("SELECT {}1{} FROM t", "(".repeat(200), ")".repeat(200));
    assert!(parse(&deep).is_ok());
}

#[test]
fn error_messages_point_at_the_offending_token() {
    let err = parse("SELECT * FRO t").unwrap_err();
    assert_eq!(err.to_string(), "syntax error near \"FRO\" at offset 9");

    let err = parse("SELECT * FROM t WHERE").unwrap_err();
    assert_eq!(
        err.to_string(),
        "syntax error: unexpected end of input at offset 21"
    );
}
