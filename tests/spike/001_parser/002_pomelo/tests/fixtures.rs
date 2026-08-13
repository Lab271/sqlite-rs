//! Fixture conformance test for the pomelo spike variant.
//!
//! Reads the shared fixtures from `tests/spike/001_parser/fixtures/`:
//!   * every statement in `valid.sql` must parse
//!   * every statement in `invalid.sql` must produce an `Err` (never panic)

use pomelo_spike::{parse, split_statements};

const VALID: &str = include_str!("../../fixtures/valid.sql");
const INVALID: &str = include_str!("../../fixtures/invalid.sql");

#[test]
fn all_valid_statements_parse() {
    let stmts = split_statements(VALID);
    assert!(!stmts.is_empty(), "no fixtures loaded");

    let mut failures = Vec::new();
    for (i, sql) in stmts.iter().enumerate() {
        match parse(sql) {
            Ok(_) => {}
            Err(e) => failures.push(format!("  [{}] {}\n      -> {}", i + 1, sql, e)),
        }
    }
    println!("valid.sql: {}/{} parsed", stmts.len() - failures.len(), stmts.len());
    assert!(
        failures.is_empty(),
        "{} of {} valid statements failed to parse:\n{}",
        failures.len(),
        stmts.len(),
        failures.join("\n")
    );
}

#[test]
fn all_invalid_statements_fail() {
    let stmts = split_statements(INVALID);
    assert!(!stmts.is_empty(), "no fixtures loaded");

    let mut accepted = Vec::new();
    for (i, sql) in stmts.iter().enumerate() {
        if parse(sql).is_ok() {
            accepted.push(format!("  [{}] {}", i + 1, sql));
        }
    }
    println!("invalid.sql: {}/{} rejected", stmts.len() - accepted.len(), stmts.len());
    assert!(
        accepted.is_empty(),
        "{} of {} invalid statements were wrongly accepted:\n{}",
        accepted.len(),
        stmts.len(),
        accepted.join("\n")
    );
}

/// Prove semantic actions produce a real tree with parse.y's precedence.
#[test]
fn precedence_and_ast_shape() {
    use pomelo_spike::ast::*;

    // 1 + 2 * 3  ==>  1 + (2 * 3)
    let stmt = parse("SELECT 1 + 2 * 3").expect("parses");
    let Stmt::Select(sel) = stmt else { panic!("expected SELECT") };
    let ResultColumn::Expr { expr, .. } = &sel.columns[0] else { panic!("expected expr column") };
    let expected = binary(
        BinaryOp::Add,
        Expr::Literal(Literal::Integer(1)),
        binary(
            BinaryOp::Mul,
            Expr::Literal(Literal::Integer(2)),
            Expr::Literal(Literal::Integer(3)),
        ),
    );
    assert_eq!(expr, &expected);

    // NOT binds tighter than AND/OR but looser than `=`: NOT a = b ==> NOT (a = b)
    let stmt = parse("DELETE FROM t WHERE NOT a = b").expect("parses");
    let Stmt::Delete { where_clause: Some(w), .. } = stmt else { panic!("expected DELETE") };
    assert_eq!(
        w,
        unary(
            UnaryOp::Not,
            binary(
                BinaryOp::Eq,
                Expr::Column { table: None, name: "a".into() },
                Expr::Column { table: None, name: "b".into() },
            )
        )
    );

    // `||` binds tighter than `*` (parse.y puts CONCAT above STAR/SLASH/REM)
    let stmt = parse("SELECT 1 * 'a' || 'b'").expect("parses");
    let Stmt::Select(sel) = stmt else { panic!("expected SELECT") };
    let ResultColumn::Expr { expr, .. } = &sel.columns[0] else { panic!("expected expr column") };
    assert!(
        matches!(expr, Expr::Binary { op: BinaryOp::Mul, .. }),
        "expected `*` at the root, got {expr:?}"
    );
}
