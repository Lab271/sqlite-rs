//! Unit tests for the V2 SELECT-core parser (spec 002-parser
//! Requirements 2-4).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use sqlite_rs::parser::ast::*;
use sqlite_rs::parser::{parse_select, ParseOutcome};

fn accept(src: &str) -> Select {
    match parse_select(src) {
        ParseOutcome::Accepted(select) => *select,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn unsupported(src: &str) -> String {
    match parse_select(src) {
        ParseOutcome::Unsupported { message, .. } => message,
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

fn invalid(src: &str) -> String {
    match parse_select(src) {
        ParseOutcome::Invalid { message, .. } => message,
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

/// Requirement 2, "Accept valid SELECT" scenario.
#[test]
fn test_accept_select_star() {
    let select = accept("SELECT * FROM t");
    assert_eq!(select.columns, vec![ResultColumn::Star]);
    assert_eq!(select.from.unwrap().name, "t");
}

/// Requirement 3, "Preserve column aliases" scenario.
#[test]
fn test_preserve_column_alias() {
    let select = accept("SELECT a AS alias");
    let printed = select.to_string();
    assert!(printed.contains("AS alias"), "printed: {printed}");
}

/// Requirement 3, "Preserve parentheses for precedence" scenario.
#[test]
fn test_preserve_parens_for_precedence() {
    let select = accept("SELECT (a + b) * c");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!("expected expr column");
    };
    let ExprKind::Binary { op, lhs, .. } = &expr.kind else {
        panic!("expected top-level Mul binary");
    };
    assert_eq!(*op, BinaryOp::Mul);
    assert!(matches!(lhs.kind, ExprKind::Paren(_)));
}

/// Requirement 4, "Error on unexpected token" scenario.
#[test]
fn test_error_on_missing_columns() {
    let message = invalid("SELECT FROM t");
    assert!(
        message.contains("expected column or expression"),
        "message: {message}"
    );
}

#[test]
fn test_where_order_by_limit_offset() {
    let select = accept("SELECT a FROM t WHERE a > 1 ORDER BY a DESC LIMIT 10 OFFSET 5");
    assert!(select.where_clause.is_some());
    assert_eq!(select.order_by.len(), 1);
    assert_eq!(select.order_by[0].desc, Some(true));
    let limit = select.limit.unwrap();
    assert!(limit.offset.is_some());
}

#[test]
fn test_limit_comma_offset_form() {
    let select = accept("SELECT a FROM t LIMIT 5, 10");
    let limit = select.limit.unwrap();
    assert!(limit.offset.is_some());
}

#[test]
fn test_distinct_and_all() {
    assert_eq!(
        accept("SELECT DISTINCT a").distinct,
        Some(Distinctness::Distinct)
    );
    assert_eq!(accept("SELECT ALL a").distinct, Some(Distinctness::All));
    assert_eq!(accept("SELECT a").distinct, None);
}

#[test]
fn test_table_star() {
    let select = accept("SELECT t.* FROM t");
    assert_eq!(
        select.columns[0],
        ResultColumn::TableStar {
            table: "t".to_string()
        }
    );
}

#[test]
fn test_qualified_column_ref() {
    let select = accept("SELECT a.b.c");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!()
    };
    assert_eq!(
        expr.kind,
        ExprKind::Column {
            catalog: Some("a".into()),
            table: Some("b".into()),
            name: "c".into(),
        }
    );
}

#[test]
fn test_function_call_distinct_and_star() {
    let select = accept("SELECT count(*), sum(DISTINCT a)");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!()
    };
    assert!(matches!(
        &expr.kind,
        ExprKind::FunctionCall {
            args: FunctionArgs::Star,
            ..
        }
    ));
    let ResultColumn::Expr { expr, .. } = &select.columns[1] else {
        panic!()
    };
    let ExprKind::FunctionCall { distinct, .. } = &expr.kind else {
        panic!()
    };
    assert!(*distinct);
}

#[test]
fn test_operator_precedence() {
    // AND binds tighter than OR: a OR (b AND c)
    let select = accept("SELECT a OR b AND c");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!()
    };
    let ExprKind::Binary { op, rhs, .. } = &expr.kind else {
        panic!("expected top level to be OR")
    };
    assert_eq!(*op, BinaryOp::Or);
    assert!(matches!(
        rhs.kind,
        ExprKind::Binary {
            op: BinaryOp::And,
            ..
        }
    ));
}

#[test]
fn test_is_null_isnull_notnull() {
    accept("SELECT a IS NULL");
    accept("SELECT a IS NOT NULL");
    accept("SELECT a ISNULL");
    accept("SELECT a NOTNULL");
    accept("SELECT a NOT NULL");
}

#[test]
fn test_between_not_between() {
    let select = accept("SELECT a BETWEEN 1 AND 10");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!()
    };
    assert!(matches!(
        expr.kind,
        ExprKind::Between { negated: false, .. }
    ));

    let select = accept("SELECT a NOT BETWEEN 1 AND 10");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!()
    };
    assert!(matches!(expr.kind, ExprKind::Between { negated: true, .. }));
}

#[test]
fn test_in_list_and_not_in() {
    accept("SELECT a IN (1, 2, 3)");
    accept("SELECT a NOT IN (1, 2, 3)");
    accept("SELECT a IN ()");
}

#[test]
fn test_like_glob_escape() {
    accept("SELECT a LIKE 'x%' ESCAPE '\\'");
    accept("SELECT a NOT GLOB '*.txt'");
}

#[test]
fn test_case_expr() {
    let select = accept("SELECT CASE a WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!()
    };
    let ExprKind::Case {
        operand,
        whens,
        else_,
    } = &expr.kind
    else {
        panic!("expected CASE")
    };
    assert!(operand.is_some());
    assert_eq!(whens.len(), 2);
    assert!(else_.is_some());
}

#[test]
fn test_cast_expr() {
    let select = accept("SELECT CAST(a AS INTEGER)");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!()
    };
    assert!(matches!(&expr.kind, ExprKind::Cast { type_name, .. } if type_name == "INTEGER"));
}

#[test]
fn test_collate() {
    accept("SELECT a COLLATE NOCASE");
    accept("SELECT a FROM t ORDER BY a COLLATE NOCASE ASC NULLS LAST");
}

#[test]
fn test_parameters() {
    accept("SELECT ?");
    accept("SELECT ?5");
    accept("SELECT :name");
    accept("SELECT @var");
    accept("SELECT $param");
}

#[test]
fn test_table_alias() {
    let select = accept("SELECT a FROM t AS x");
    assert_eq!(select.from.unwrap().alias.as_deref(), Some("x"));
    let select = accept("SELECT a FROM t x");
    assert_eq!(select.from.unwrap().alias.as_deref(), Some("x"));
}

// ---- three-way outcome: unsupported ------------------------------------

#[test]
fn test_unsupported_join() {
    let msg = unsupported("SELECT * FROM a JOIN b");
    assert!(msg.contains("JOIN"), "message: {msg}");
}

#[test]
fn test_unsupported_comma_join() {
    let msg = unsupported("SELECT * FROM a, b");
    assert!(msg.contains("JOIN"), "message: {msg}");
}

#[test]
fn test_unsupported_compound_select() {
    let msg = unsupported("SELECT a UNION SELECT b");
    assert!(msg.contains("compound"), "message: {msg}");
}

#[test]
fn test_unsupported_group_by() {
    let msg = unsupported("SELECT a FROM t GROUP BY a");
    assert!(msg.contains("GROUP BY"), "message: {msg}");
}

#[test]
fn test_unsupported_subquery() {
    let msg = unsupported("SELECT (SELECT 1)");
    assert!(msg.contains("subquery"), "message: {msg}");
}

#[test]
fn test_unsupported_cte() {
    let msg = unsupported("WITH cte AS (SELECT 1) SELECT * FROM cte");
    assert!(msg.contains("CTE"), "message: {msg}");
}

// ---- three-way outcome: invalid ----------------------------------------

#[test]
fn test_invalid_missing_from_table() {
    invalid("SELECT a FROM");
}

#[test]
fn test_invalid_unterminated_paren() {
    invalid("SELECT (a + b");
}

#[test]
fn test_invalid_case_without_when() {
    invalid("SELECT CASE a END");
}

// ---- roundtrip: parse -> print -> parse is a fixpoint -------------------

#[test]
fn test_roundtrip_fixpoint() {
    let cases = [
        "SELECT * FROM t",
        "SELECT a AS x, b FROM t WHERE a > 1 AND b < 2",
        "SELECT (a + b) * c FROM t",
        "SELECT a FROM t ORDER BY a DESC, b ASC NULLS LAST LIMIT 10 OFFSET 5",
        "SELECT a NOT BETWEEN 1 AND 10",
        "SELECT a IN (1, 2, 3)",
        "SELECT CASE a WHEN 1 THEN 'x' ELSE 'y' END",
        "SELECT CAST(a AS TEXT)",
        "SELECT count(DISTINCT a), sum(*)",
    ];
    for src in cases {
        let select1 = accept(src);
        let printed1 = select1.to_string();
        let select2 = accept(&printed1);
        let printed2 = select2.to_string();
        assert_eq!(printed1, printed2, "not a fixpoint for {src:?}");
    }
}

// ---- pathological input: deep nesting must not overflow the stack -------

#[test]
fn test_deeply_nested_parens_rejected_not_crashed() {
    let nested = format!("SELECT {}1{}", "(".repeat(10_000), ")".repeat(10_000));
    match parse_select(&nested) {
        ParseOutcome::Invalid { .. } => {}
        other => panic!("expected Invalid for pathologically nested input, got {other:?}"),
    }
}

#[test]
fn test_deeply_nested_not_rejected_not_crashed() {
    let nested = format!("SELECT {}1", "NOT ".repeat(10_000));
    match parse_select(&nested) {
        ParseOutcome::Invalid { .. } => {}
        other => panic!("expected Invalid for pathologically nested input, got {other:?}"),
    }
}
