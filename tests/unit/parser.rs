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
use sqlite_rs::parser::{parse_select, parse_update, ParseOutcome};

fn accept(src: &str) -> Select {
    match parse_select(src) {
        ParseOutcome::Accepted(select) => *select,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn accept_update(src: &str) -> Update {
    match parse_update(src) {
        ParseOutcome::Accepted(update) => *update,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn unsupported_update(src: &str) -> String {
    match parse_update(src) {
        ParseOutcome::Unsupported { message, .. } => message,
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

fn invalid_update(src: &str) -> String {
    match parse_update(src) {
        ParseOutcome::Invalid { message, .. } => message,
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

/// Issue #190: basic `UPDATE ... SET ... WHERE ...`.
#[test]
fn test_accept_update_basic() {
    let update = accept_update("UPDATE t1 SET x=1 WHERE x>0");
    assert_eq!(update.table, "t1");
    assert_eq!(update.or_action, None);
    assert_eq!(update.assignments.len(), 1);
    assert_eq!(update.assignments[0].columns, vec!["x".to_string()]);
    assert!(update.where_clause.is_some());
}

/// Issue #190: multiple assignments, no WHERE.
#[test]
fn test_accept_update_multiple_assignments_no_where() {
    let update = accept_update("UPDATE t1 SET x=3, x=4, x=5");
    assert_eq!(update.assignments.len(), 3);
    assert!(update.where_clause.is_none());
}

/// Issue #190: `UPDATE OR REPLACE`/`OR IGNORE` conflict actions.
#[test]
fn test_accept_update_or_conflict_actions() {
    let update = accept_update("UPDATE OR IGNORE t1 SET a=1000");
    assert_eq!(update.or_action, Some(ConflictAction::Ignore));

    let update = accept_update("UPDATE OR REPLACE t1 SET a=1001");
    assert_eq!(update.or_action, Some(ConflictAction::Replace));

    let update = accept_update("UPDATE OR ROLLBACK t1 SET a=1");
    assert_eq!(update.or_action, Some(ConflictAction::Rollback));

    let update = accept_update("UPDATE OR ABORT t1 SET a=1");
    assert_eq!(update.or_action, Some(ConflictAction::Abort));

    let update = accept_update("UPDATE OR FAIL t1 SET a=1");
    assert_eq!(update.or_action, Some(ConflictAction::Fail));
}

/// Issue #190: tuple SET form expands to one Assignment per column.
#[test]
fn test_accept_update_tuple_set_form() {
    let update = accept_update("UPDATE t1 SET (x, y) = (1, 2)");
    assert_eq!(update.assignments.len(), 2);
    assert_eq!(update.assignments[0].columns, vec!["x".to_string()]);
    assert_eq!(update.assignments[1].columns, vec!["y".to_string()]);
}

/// Issue #190: tuple SET form with mismatched arity is a syntax error.
#[test]
fn test_reject_update_tuple_set_arity_mismatch() {
    invalid_update("UPDATE t1 SET (x, y) = (1, 2, 3)");
}

/// Issue #190: tuple SET form with a subquery RHS is not yet supported.
#[test]
fn test_reject_update_tuple_set_subquery_rhs_unsupported() {
    unsupported_update("UPDATE t1 SET (x, y) = (SELECT a, b FROM t)");
}

/// Issue #190: WHERE reuses the existing expr parser, including subqueries
/// that the V2 expr grammar doesn't support yet.
#[test]
fn test_reject_update_where_subquery_unsupported() {
    // #238 made `IN (SELECT ...)` a generic WHERE-clause production
    // shared across SELECT/UPDATE/DELETE, so this now parses; UPDATE's
    // own codegen (untouched by #238) still doesn't thread a catalog
    // through to resolve it, so it fails one stage later instead.
    let update = accept_update("UPDATE t1 SET x=1 WHERE x IN (SELECT x FROM t)");
    assert!(matches!(
        update.where_clause.map(|w| w.kind),
        Some(ExprKind::InSubquery { .. })
    ));
}

/// Issue #190: missing SET keyword is a syntax error.
#[test]
fn test_reject_update_missing_set() {
    invalid_update("UPDATE t1 x=1");
}

/// Issue #224: trailing UNION after a valid UPDATE is rejected at
/// `expect_end`, not during statement parsing itself.
#[test]
fn test_reject_update_trailing_compound_unsupported() {
    unsupported_update("UPDATE t1 SET x=1 UNION SELECT 1");
}

/// Issue #224: trailing garbage after a valid UPDATE is rejected at
/// `expect_end` as `Invalid`.
#[test]
fn test_reject_update_trailing_garbage_invalid() {
    invalid_update("UPDATE t1 SET x=1 EXTRA");
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
    assert_eq!(select.from.unwrap().first.name, "t");
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

/// Spike #59 found this: `replace`/`glob` etc. tokenize as keywords, but
/// SQLite still accepts them as function names when followed by `(` —
/// only CASE/CAST/EXISTS/CURRENT_* are true reserved words here.
#[test]
fn test_keyword_named_function_call() {
    let select = accept("SELECT replace('abcabc','a','Z')");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!()
    };
    let ExprKind::FunctionCall { name, args, .. } = &expr.kind else {
        panic!("expected a function call, got {:?}", expr.kind)
    };
    assert_eq!(name, "REPLACE");
    assert!(matches!(args, FunctionArgs::List(list) if list.len() == 3));
}

/// Spike #59 finding: `9223372036854775808` has no positive i64 form so
/// the tokenizer folds it to a Float; negated it is exactly i64::MIN,
/// which SQLite parses as an INTEGER literal, not a REAL.
#[test]
fn test_negative_i64_min_literal_stays_integer() {
    let select = accept("SELECT -9223372036854775808");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!()
    };
    assert_eq!(expr.kind, ExprKind::Literal(Literal::Integer(i64::MIN)));
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
    assert_eq!(select.from.unwrap().first.alias.as_deref(), Some("x"));
    let select = accept("SELECT a FROM t x");
    assert_eq!(select.from.unwrap().first.alias.as_deref(), Some("x"));
}

// ---- three-way outcome: unsupported ------------------------------------

#[test]
fn test_unsupported_join() {
    // A bare `JOIN` with no `ON`/`USING` — real SQL (equivalent to a
    // constraint-less cross join), but outside #237's `ON`-qualified
    // MVP scope.
    let msg = unsupported("SELECT * FROM a JOIN b");
    assert!(msg.contains("JOIN"), "message: {msg}");
}

#[test]
fn test_unsupported_comma_join() {
    let msg = unsupported("SELECT * FROM a, b");
    assert!(msg.contains("JOIN"), "message: {msg}");
}

/// #237: `JOIN`/`INNER JOIN ... ON`, `LEFT [OUTER] JOIN ... ON`, and
/// `CROSS JOIN` (no `ON`) all parse into a `FromClause` with one `Join`
/// per join step, in source order.
#[test]
fn test_accept_inner_join_with_on() {
    let select = accept("SELECT * FROM a JOIN b ON a.x = b.y");
    let from = select.from.unwrap();
    assert_eq!(from.first.name, "a");
    assert_eq!(from.joins.len(), 1);
    assert_eq!(from.joins[0].op, JoinOp::Inner);
    assert_eq!(from.joins[0].table.name, "b");
    assert!(matches!(
        from.joins[0].constraint,
        Some(JoinConstraint::On(_))
    ));
}

#[test]
fn test_accept_explicit_inner_join_with_on() {
    let select = accept("SELECT * FROM a INNER JOIN b ON a.x = b.y");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Inner);
}

#[test]
fn test_accept_left_join_with_on() {
    let select = accept("SELECT * FROM a LEFT JOIN b ON a.x = b.y");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Left);
}

#[test]
fn test_accept_left_outer_join_with_on() {
    let select = accept("SELECT * FROM a LEFT OUTER JOIN b ON a.x = b.y");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Left);
}

#[test]
fn test_accept_cross_join_without_on() {
    let select = accept("SELECT * FROM a CROSS JOIN b");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Cross);
    assert!(from.joins[0].constraint.is_none());
}

#[test]
fn test_accept_multi_way_join_chain() {
    let select = accept("SELECT * FROM a JOIN b ON a.x = b.y LEFT JOIN c ON b.z = c.w");
    let from = select.from.unwrap();
    assert_eq!(from.joins.len(), 2);
    assert_eq!(from.joins[0].op, JoinOp::Inner);
    assert_eq!(from.joins[0].table.name, "b");
    assert_eq!(from.joins[1].op, JoinOp::Left);
    assert_eq!(from.joins[1].table.name, "c");
}

#[test]
fn test_unsupported_join_using() {
    let msg = unsupported("SELECT * FROM a JOIN b USING (x)");
    assert!(msg.contains("USING"), "message: {msg}");
}

#[test]
fn test_unsupported_natural_join() {
    let msg = unsupported("SELECT * FROM a NATURAL JOIN b");
    assert!(msg.contains("NATURAL"), "message: {msg}");
}

#[test]
fn test_unsupported_right_join() {
    let msg = unsupported("SELECT * FROM a RIGHT JOIN b ON a.x = b.y");
    assert!(msg.contains("RIGHT"), "message: {msg}");
}

#[test]
fn test_unsupported_full_join() {
    let msg = unsupported("SELECT * FROM a FULL JOIN b ON a.x = b.y");
    assert!(msg.contains("FULL"), "message: {msg}");
}

#[test]
fn test_unsupported_cross_join_with_on() {
    let msg = unsupported("SELECT * FROM a CROSS JOIN b ON a.x = b.y");
    assert!(msg.contains("CROSS"), "message: {msg}");
}

#[test]
fn test_unsupported_compound_select() {
    let msg = unsupported("SELECT a UNION SELECT b");
    assert!(msg.contains("compound"), "message: {msg}");
}

#[test]
fn test_unsupported_having_without_group_by() {
    // Parses (`HAVING` alone isn't a syntax error), but this V4 slice
    // only accepts `HAVING` paired with `GROUP BY` — see #239.
    let msg = unsupported("SELECT count(*) FROM t HAVING count(*) > 1");
    assert!(msg.contains("HAVING"), "message: {msg}");
}

#[test]
fn test_group_by_single_column() {
    let select = accept("SELECT a FROM t GROUP BY a");
    assert_eq!(select.group_by.len(), 1);
    assert!(select.having.is_none());
}

#[test]
fn test_group_by_multiple_columns() {
    let select = accept("SELECT a, b FROM t GROUP BY a, b");
    assert_eq!(select.group_by.len(), 2);
}

#[test]
fn test_group_by_with_expression() {
    let select = accept("SELECT a FROM t GROUP BY a + 1");
    assert_eq!(select.group_by.len(), 1);
    assert!(matches!(
        select.group_by[0].kind,
        ExprKind::Binary {
            op: BinaryOp::Add,
            ..
        }
    ));
}

#[test]
fn test_group_by_having() {
    let select = accept("SELECT a, count(*) FROM t GROUP BY a HAVING count(*) > 1");
    assert_eq!(select.group_by.len(), 1);
    assert!(select.having.is_some());
}

#[test]
fn test_scalar_subquery_parses() {
    // #238: scalar subqueries are now a supported expression form.
    let select = accept("SELECT (SELECT 1)");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!(
            "expected an Expr result column, got {:?}",
            select.columns[0]
        );
    };
    assert!(matches!(expr.kind, ExprKind::Subquery(_)));
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
        // Select-level DISTINCT/ALL — the top of Select::fmt.
        "SELECT DISTINCT a FROM t",
        "SELECT ALL a FROM t",
        // ResultColumn::TableStar and TableRef alias.
        "SELECT t.* FROM t AS x",
        // Qualified/catalog column references.
        "SELECT db.t.a FROM t",
        // Multi-arg function call (the i > 0 comma branch).
        "SELECT foo(a, b, c)",
        // All four UnaryOp variants.
        "SELECT NOT a",
        "SELECT +a",
        "SELECT -a",
        "SELECT ~a",
        // IS / IS NOT, ISNULL / NOTNULL.
        "SELECT a IS b",
        "SELECT a IS NOT b",
        "SELECT a ISNULL",
        "SELECT a NOTNULL",
        // LIKE / GLOB, with and without ESCAPE.
        "SELECT a LIKE 'x%'",
        "SELECT a NOT LIKE 'x%' ESCAPE '\\'",
        "SELECT a GLOB '*x*'",
        // COLLATE.
        "SELECT a COLLATE nocase",
        // The remaining BinaryOp variants (AND/</>/+/-/* already covered above).
        "SELECT a OR b",
        "SELECT a = b",
        "SELECT a != b",
        "SELECT a <= b",
        "SELECT a >= b",
        "SELECT a & b",
        "SELECT a | b",
        "SELECT a << b",
        "SELECT a >> b",
        "SELECT a / b",
        "SELECT a % b",
        "SELECT a || b",
        // Literal variants: Float, Blob, Null, True, False.
        "SELECT 1.5",
        "SELECT X'DEADBEEF'",
        "SELECT NULL",
        "SELECT TRUE",
        "SELECT FALSE",
        // Every ParamKind.
        "SELECT ?",
        "SELECT ?1",
        "SELECT :name",
        "SELECT @name",
        "SELECT $name",
        // OrderingTerm's implicit order and NULLS FIRST branches.
        "SELECT a FROM t ORDER BY a",
        "SELECT a FROM t ORDER BY a NULLS FIRST",
        // CASE without an operand and without ELSE.
        "SELECT CASE WHEN a THEN 1 END",
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

// ---- #238: subquery expressions -----------------------------------------

#[test]
fn test_scalar_subquery_in_where_clause_parses() {
    let select = accept("SELECT id FROM t WHERE x = (SELECT y FROM u)");
    let Some(where_clause) = select.where_clause else {
        panic!("expected a WHERE clause");
    };
    let ExprKind::Binary { rhs, .. } = where_clause.kind else {
        panic!("expected a Binary comparison, got {:?}", where_clause.kind);
    };
    assert!(matches!(rhs.kind, ExprKind::Subquery(_)));
}

#[test]
fn test_in_subquery_parses() {
    let select = accept("SELECT id FROM t WHERE id IN (SELECT a_id FROM other)");
    let Some(where_clause) = select.where_clause else {
        panic!("expected a WHERE clause");
    };
    match where_clause.kind {
        ExprKind::InSubquery { negated, .. } => assert!(!negated),
        other => panic!("expected InSubquery, got {other:?}"),
    }
}

#[test]
fn test_not_in_subquery_parses() {
    let select = accept("SELECT id FROM t WHERE id NOT IN (SELECT a_id FROM other)");
    let Some(where_clause) = select.where_clause else {
        panic!("expected a WHERE clause");
    };
    match where_clause.kind {
        ExprKind::InSubquery { negated, .. } => assert!(negated),
        other => panic!("expected InSubquery, got {other:?}"),
    }
}

#[test]
fn test_exists_subquery_parses() {
    let select = accept("SELECT id FROM t WHERE EXISTS (SELECT 1 FROM other)");
    let Some(where_clause) = select.where_clause else {
        panic!("expected a WHERE clause");
    };
    match where_clause.kind {
        ExprKind::Exists { negated, .. } => assert!(!negated),
        other => panic!("expected Exists, got {other:?}"),
    }
}

#[test]
fn test_not_exists_subquery_parses_as_exists_negated_not_generic_not() {
    let select = accept("SELECT id FROM t WHERE NOT EXISTS (SELECT 1 FROM other)");
    let Some(where_clause) = select.where_clause else {
        panic!("expected a WHERE clause");
    };
    // Must compile directly to `Exists { negated: true, .. }`, not a
    // generic `Unary { op: Not, expr: Exists { negated: false, .. } }`
    // wrapper — mirrors how `NOT IN`/`NOT BETWEEN`/`NOT LIKE` are their
    // own negated variant rather than a `NOT` wrapper.
    match where_clause.kind {
        ExprKind::Exists { negated, .. } => assert!(negated),
        other => panic!("expected Exists {{ negated: true }}, got {other:?}"),
    }
}

#[test]
fn test_subquery_in_select_list_parses() {
    let select = accept("SELECT (SELECT 1)");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!("expected an Expr result column");
    };
    assert!(matches!(expr.kind, ExprKind::Subquery(_)));
}

#[test]
fn test_exists_requires_a_select() {
    unsupported("SELECT id FROM t WHERE EXISTS (1, 2)");
}

#[test]
fn test_compound_select_inside_subquery_is_unsupported_not_invalid() {
    let msg = unsupported("SELECT id FROM t WHERE id IN (SELECT a FROM u UNION SELECT b FROM v)");
    assert!(msg.contains("compound"), "message: {msg}");
}

#[test]
fn test_subqueries_in_from_still_unsupported() {
    unsupported("SELECT * FROM (SELECT * FROM t) AS sub");
}

#[test]
fn test_quantified_any_comparison_still_unsupported_or_invalid() {
    match parse_select("SELECT id FROM t WHERE x > ANY (SELECT y FROM u)") {
        ParseOutcome::Unsupported { .. } | ParseOutcome::Invalid { .. } => {}
        other => panic!("expected ANY comparisons to fail to parse cleanly, got {other:?}"),
    }
}

#[test]
fn test_quantified_all_comparison_still_unsupported_or_invalid() {
    match parse_select("SELECT id FROM t WHERE x > ALL (SELECT y FROM u)") {
        ParseOutcome::Unsupported { .. } | ParseOutcome::Invalid { .. } => {}
        other => panic!("expected ALL comparisons to fail to parse cleanly, got {other:?}"),
    }
}
