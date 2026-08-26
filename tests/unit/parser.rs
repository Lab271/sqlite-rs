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
use sqlite_rs::parser::{parse_explain, parse_select, parse_update, ParseOutcome};

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
    // shared across SELECT/UPDATE/DELETE. #251 threaded a table catalog
    // through `compile_update`, so this now compiles too — see
    // `tests/corpus/subquery_test.rs`'s `update_where_in_subquery_matches_oracle`.
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

/// Once control returns to `parse_select`'s own top-level `expect_end`
/// (rather than a nested compound-SELECT's), a mixed `UNION` then
/// `INTERSECT` leaves the `INTERSECT` as an unconsumed trailing token —
/// `expect_end` classifies that as unimplemented-but-recognized, not a
/// plain syntax error, matching `test_reject_update_trailing_compound_unsupported`'s
/// shape for UPDATE.
#[test]
fn test_reject_select_trailing_compound_unsupported() {
    unsupported("SELECT 1 UNION SELECT 2 INTERSECT SELECT 3");
}

/// Trailing garbage after an otherwise-complete SELECT is rejected at
/// `parse_select`'s own `expect_end`, not from within
/// `parse_select_stmt` itself.
#[test]
fn test_reject_select_trailing_garbage_invalid() {
    invalid("SELECT 1)");
}

fn unsupported_explain(src: &str) -> String {
    match parse_explain(src) {
        ParseOutcome::Unsupported { message, .. } => message,
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

fn invalid_explain(src: &str) -> String {
    match parse_explain(src) {
        ParseOutcome::Invalid { message, .. } => message,
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

/// #538: bare `EXPLAIN` (no `QUERY PLAN`) parses the same as `EXPLAIN
/// QUERY PLAN` — `query_plan` distinguishes the two at render time.
#[test]
fn test_accept_bare_explain() {
    match parse_explain("EXPLAIN SELECT 1") {
        ParseOutcome::Accepted(explain) => assert!(!explain.query_plan),
        other => panic!("expected accepted, got {other:?}"),
    }
}

/// `EXPLAIN QUERY PLAN` followed by anything other than a `SELECT` is
/// a syntax error at the nested `parse_select_stmt` call.
#[test]
fn test_invalid_explain_query_plan_missing_select() {
    invalid_explain("EXPLAIN QUERY PLAN");
}

/// Trailing garbage after an otherwise-complete `EXPLAIN QUERY PLAN
/// select-stmt` is rejected at `parse_explain`'s own `expect_end`.
#[test]
fn test_invalid_explain_trailing_garbage() {
    invalid_explain("EXPLAIN QUERY PLAN SELECT 1)");
}

/// Same shared `expect_end` classification as
/// `test_reject_select_trailing_compound_unsupported`, reached this
/// time via `EXPLAIN QUERY PLAN`.
#[test]
fn test_unsupported_explain_trailing_compound() {
    unsupported_explain("EXPLAIN QUERY PLAN SELECT 1 UNION SELECT 2 INTERSECT SELECT 3");
}

/// Requirement 2, "Accept valid SELECT" scenario.
#[test]
fn test_accept_select_star() {
    let select = accept("SELECT * FROM t");
    assert_eq!(select.columns, vec![ResultColumn::Star]);
    assert_eq!(select.from.unwrap().first.name(), Some("t"));
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
fn test_accept_comma_join() {
    // #250: `FROM a, b` is ANSI comma-join sugar for an unconstrained
    // CROSS JOIN, synthesized as such in the same `joins` chain.
    let select = accept("SELECT * FROM a, b");
    let from = select.from.unwrap();
    assert_eq!(from.joins.len(), 1);
    assert_eq!(from.joins[0].op, JoinOp::Cross);
    assert!(from.joins[0].constraint.is_none());
    assert!(!from.joins[0].natural);
    assert_eq!(from.joins[0].table.name(), Some("b"));
}

#[test]
fn test_accept_comma_join_mixed_with_explicit_join() {
    // A leading comma and an explicit JOIN keyword can appear in the
    // same FROM clause.
    let select = accept("SELECT * FROM a, b JOIN c ON b.x = c.y");
    let from = select.from.unwrap();
    assert_eq!(from.joins.len(), 2);
    assert_eq!(from.joins[0].op, JoinOp::Cross);
    assert_eq!(from.joins[1].op, JoinOp::Inner);
}

/// #237: `JOIN`/`INNER JOIN ... ON`, `LEFT [OUTER] JOIN ... ON`, and
/// `CROSS JOIN` (no `ON`) all parse into a `FromClause` with one `Join`
/// per join step, in source order.
#[test]
fn test_accept_inner_join_with_on() {
    let select = accept("SELECT * FROM a JOIN b ON a.x = b.y");
    let from = select.from.unwrap();
    assert_eq!(from.first.name(), Some("a"));
    assert_eq!(from.joins.len(), 1);
    assert_eq!(from.joins[0].op, JoinOp::Inner);
    assert_eq!(from.joins[0].table.name(), Some("b"));
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
    assert_eq!(from.joins[0].table.name(), Some("b"));
    assert_eq!(from.joins[1].op, JoinOp::Left);
    assert_eq!(from.joins[1].table.name(), Some("c"));
}

#[test]
fn test_accept_join_using() {
    let select = accept("SELECT * FROM a JOIN b USING (x)");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Inner);
    assert_eq!(
        from.joins[0].constraint,
        Some(JoinConstraint::Using(vec!["x".to_string()]))
    );
}

#[test]
fn test_accept_join_using_multiple_columns() {
    let select = accept("SELECT * FROM a JOIN b USING (x, y)");
    let from = select.from.unwrap();
    assert_eq!(
        from.joins[0].constraint,
        Some(JoinConstraint::Using(vec![
            "x".to_string(),
            "y".to_string()
        ]))
    );
}

#[test]
fn test_accept_cross_join_using() {
    let select = accept("SELECT * FROM a CROSS JOIN b USING (x)");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Cross);
    assert_eq!(
        from.joins[0].constraint,
        Some(JoinConstraint::Using(vec!["x".to_string()]))
    );
}

#[test]
fn test_accept_natural_join() {
    let select = accept("SELECT * FROM a NATURAL JOIN b");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Inner);
    assert!(from.joins[0].natural);
    assert!(from.joins[0].constraint.is_none());
}

#[test]
fn test_accept_natural_left_join() {
    let select = accept("SELECT * FROM a NATURAL LEFT JOIN b");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Left);
    assert!(from.joins[0].natural);
}

#[test]
fn test_unsupported_natural_cross_join() {
    // NATURAL CROSS JOIN is not valid SQLite grammar.
    let msg = unsupported("SELECT * FROM a NATURAL CROSS JOIN b");
    assert!(msg.contains("NATURAL"), "message: {msg}");
}

#[test]
fn test_accept_right_join() {
    let select = accept("SELECT * FROM a RIGHT JOIN b ON a.x = b.y");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Right);
    assert!(matches!(
        from.joins[0].constraint,
        Some(JoinConstraint::On(_))
    ));
}

#[test]
fn test_accept_right_outer_join() {
    let select = accept("SELECT * FROM a RIGHT OUTER JOIN b ON a.x = b.y");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Right);
}

#[test]
fn test_accept_full_join() {
    let select = accept("SELECT * FROM a FULL JOIN b ON a.x = b.y");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Full);
}

#[test]
fn test_accept_full_outer_join() {
    let select = accept("SELECT * FROM a FULL OUTER JOIN b ON a.x = b.y");
    let from = select.from.unwrap();
    assert_eq!(from.joins[0].op, JoinOp::Full);
}

#[test]
fn test_unsupported_cross_join_with_on() {
    let msg = unsupported("SELECT * FROM a CROSS JOIN b ON a.x = b.y");
    assert!(msg.contains("CROSS"), "message: {msg}");
}

/// #377: `INTERSECT`/`EXCEPT` remain unsupported — only `UNION`/`UNION
/// ALL` are implemented for V6.1.
#[test]
fn test_unsupported_compound_select() {
    let msg = unsupported("SELECT a INTERSECT SELECT b");
    assert!(msg.contains("compound"), "message: {msg}");
}

/// Window functions (`OVER`/`FILTER`) are deferred to V9 (drop-order 4,
/// see `tests/tiers/tier3.rs::t3_modern_sql_upsert_returning_windows`) —
/// deliberately `Unsupported`, not `Invalid`, matching the same
/// not-yet-implemented-but-syntactically-known pattern as compound
/// SELECT's `INTERSECT`/`EXCEPT` above.
#[test]
fn test_unsupported_window_function() {
    let msg = unsupported("SELECT row_number() OVER (ORDER BY x) FROM t");
    assert!(msg.contains("window"), "message: {msg}");
}

#[test]
fn test_having_without_group_by_is_accepted() {
    // #287: `HAVING` with no `GROUP BY` now filters the implicit
    // whole-table group's aggregate result — previously (#239) this
    // V4 slice only accepted `HAVING` paired with `GROUP BY`.
    let select = accept("SELECT count(*) FROM t HAVING count(*) > 1");
    assert!(select.group_by.is_empty());
    assert!(select.having.is_some());
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
fn test_unsupported_intersect() {
    let msg = unsupported("SELECT a FROM t INTERSECT SELECT b FROM u");
    assert!(msg.contains("INTERSECT"), "message: {msg}");
}

#[test]
fn test_unsupported_except() {
    let msg = unsupported("SELECT a FROM t EXCEPT SELECT b FROM u");
    assert!(msg.contains("EXCEPT"), "message: {msg}");
}

/// #240: `UNION ALL` parses into `Select::compound`.
#[test]
fn test_accept_union_all() {
    let select = accept("SELECT a FROM t UNION ALL SELECT b FROM u");
    assert_eq!(select.compound.len(), 1);
    assert_eq!(select.compound[0].op, CompoundOp::UnionAll);
    assert!(select.compound[0].from.is_some());
}

/// #240: multiple `UNION ALL` arms chain into one `compound` vec.
#[test]
fn test_accept_multiple_union_all_arms() {
    let select = accept("SELECT a FROM t UNION ALL SELECT b FROM u UNION ALL SELECT c FROM v");
    assert_eq!(select.compound.len(), 2);
}

/// #240: ORDER BY/LIMIT bind to the whole compound statement, not any
/// one arm.
#[test]
fn test_accept_union_all_with_trailing_order_by_limit() {
    let select = accept("SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY a LIMIT 1");
    assert_eq!(select.compound.len(), 1);
    assert_eq!(select.order_by.len(), 1);
    assert!(select.limit.is_some());
}

/// #377: plain `UNION` parses into `Select::compound` with
/// `CompoundOp::Union`.
#[test]
fn test_accept_union() {
    let select = accept("SELECT a FROM t UNION SELECT b FROM u");
    assert_eq!(select.compound.len(), 1);
    assert_eq!(select.compound[0].op, CompoundOp::Union);
    assert!(select.compound[0].from.is_some());
}

/// #377: multiple `UNION` arms chain into one `compound` vec, same as
/// `UNION ALL` (#240).
#[test]
fn test_accept_multiple_union_arms() {
    let select = accept("SELECT a FROM t UNION SELECT b FROM u UNION SELECT c FROM v");
    assert_eq!(select.compound.len(), 2);
    assert_eq!(select.compound[0].op, CompoundOp::Union);
    assert_eq!(select.compound[1].op, CompoundOp::Union);
}

/// #377: `UNION` and `UNION ALL` arms can be mixed in one compound
/// statement — each arm carries its own `op`.
#[test]
fn test_accept_mixed_union_and_union_all_arms() {
    let select = accept("SELECT a FROM t UNION SELECT b FROM u UNION ALL SELECT c FROM v");
    assert_eq!(select.compound.len(), 2);
    assert_eq!(select.compound[0].op, CompoundOp::Union);
    assert_eq!(select.compound[1].op, CompoundOp::UnionAll);
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

// Non-recursive `WITH` (CTEs) is now supported (#375) — see the
// "#375: non-recursive WITH clause (CTEs)" test block below.
// `WITH RECURSIVE` remains unsupported: `test_with_recursive_is_unsupported`.

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
        // #250: NATURAL/RIGHT/FULL/USING/comma-join round-trip.
        "SELECT * FROM a NATURAL JOIN b",
        "SELECT * FROM a NATURAL LEFT JOIN b",
        "SELECT * FROM a RIGHT JOIN b ON a.x = b.y",
        "SELECT * FROM a FULL JOIN b ON a.x = b.y",
        "SELECT * FROM a JOIN b USING (x, y)",
        "SELECT * FROM a, b",
        "SELECT * FROM a, b JOIN c ON b.x = c.y",
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

/// #377: `INTERSECT`/`EXCEPT` remain unsupported inside a subquery too
/// — plain `UNION` (unlike `UNION ALL` since #240) is now accepted
/// here the same way it is at the top level.
#[test]
fn test_compound_select_inside_subquery_is_unsupported_not_invalid() {
    let msg =
        unsupported("SELECT id FROM t WHERE id IN (SELECT a FROM u INTERSECT SELECT b FROM v)");
    assert!(msg.contains("compound"), "message: {msg}");
}

/// #377: plain `UNION` inside an `IN (...)` subquery parses like
/// `UNION ALL` already did (#240) — no special-casing by op.
#[test]
fn test_union_inside_subquery_parses() {
    accept("SELECT id FROM t WHERE id IN (SELECT a FROM u UNION SELECT b FROM v)");
}

#[test]
fn test_subquery_in_from_parses() {
    let select = accept("SELECT * FROM (SELECT id FROM t WHERE x > 0) AS sub");
    let from = select.from.unwrap();
    assert_eq!(from.first.alias.as_deref(), Some("sub"));
    match &from.first.kind {
        TableRefKind::Subquery(subquery) => {
            assert_eq!(subquery.columns.len(), 1);
            assert!(subquery.where_clause.is_some());
            let inner_from = subquery.from.as_ref().unwrap();
            assert_eq!(inner_from.first.name(), Some("t"));
        }
        other => panic!("expected a subquery TableRef, got {other:?}"),
    }
}

#[test]
fn test_subquery_in_from_without_alias_is_unsupported() {
    let msg = unsupported("SELECT * FROM (SELECT * FROM t)");
    assert!(msg.contains("alias"), "message: {msg}");
}

#[test]
fn test_table_valued_function_in_from_still_unsupported() {
    unsupported("SELECT * FROM pragma_table_info('t')");
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

// ---- #251: multi-column IN (subquery) ------------------------------------

#[test]
fn test_multi_column_in_subquery_parses() {
    let select = accept("SELECT id FROM t WHERE (a, b) IN (SELECT x, y FROM u)");
    let Some(where_clause) = select.where_clause else {
        panic!("expected a WHERE clause");
    };
    match where_clause.kind {
        ExprKind::InSubqueryMulti { exprs, negated, .. } => {
            assert!(!negated);
            assert_eq!(exprs.len(), 2);
        }
        other => panic!("expected InSubqueryMulti, got {other:?}"),
    }
}

#[test]
fn test_multi_column_not_in_subquery_parses() {
    let select = accept("SELECT id FROM t WHERE (a, b) NOT IN (SELECT x, y FROM u)");
    let Some(where_clause) = select.where_clause else {
        panic!("expected a WHERE clause");
    };
    match where_clause.kind {
        ExprKind::InSubqueryMulti { negated, .. } => assert!(negated),
        other => panic!("expected InSubqueryMulti, got {other:?}"),
    }
}

/// A plain grouping paren around a single expression must still parse
/// as `ExprKind::Paren`, not be swept up by the tuple-`IN` speculative
/// parse (arity < 2 rolls back to the normal path).
#[test]
fn test_single_paren_expr_still_parses_as_paren_not_tuple() {
    let select = accept("SELECT (1 + 2) FROM t");
    let ResultColumn::Expr { expr, .. } = &select.columns[0] else {
        panic!("expected an Expr result column");
    };
    assert!(matches!(expr.kind, ExprKind::Paren(_)));
}

/// A bare parenthesized expr-list not followed by IN/NOT IN isn't valid
/// SQLite syntax outside that context — the speculative tuple-IN parse
/// rewinds and the normal single-expr path reports the trailing comma
/// as a clean parse error, not a panic.
#[test]
fn test_bare_tuple_without_in_is_invalid() {
    invalid("SELECT (a, b) FROM t");
}

/// #377: `INTERSECT`/`EXCEPT` remain unsupported inside a multi-column
/// `IN (...)` subquery too.
#[test]
fn test_multi_column_in_rejects_compound_subquery() {
    let msg =
        unsupported("SELECT id FROM t WHERE (a, b) IN (SELECT x, y FROM u INTERSECT SELECT 1, 2)");
    assert!(msg.contains("compound"), "message: {msg}");
}

// ---- #375: non-recursive WITH clause (CTEs) --------------------------

/// A single `WITH name AS (...)` prefix parses, and `with_clause` carries
/// exactly one `CommonTableExpr` with no explicit column list.
#[test]
fn test_with_clause_single_cte() {
    let select = accept("WITH cte AS (SELECT 1) SELECT * FROM cte");
    let with_clause = select.with_clause.as_ref().expect("expected a WITH clause");
    assert_eq!(with_clause.ctes.len(), 1);
    assert_eq!(with_clause.ctes[0].name, "cte");
    assert_eq!(with_clause.ctes[0].columns, None);
}

/// Multiple comma-separated CTEs in one WITH clause.
#[test]
fn test_with_clause_multiple_ctes() {
    let select = accept("WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM a, b");
    let with_clause = select.with_clause.as_ref().expect("expected a WITH clause");
    assert_eq!(with_clause.ctes.len(), 2);
    assert_eq!(with_clause.ctes[0].name, "a");
    assert_eq!(with_clause.ctes[1].name, "b");
}

/// A CTE with an explicit column list: `cte(x, y) AS (...)`.
#[test]
fn test_with_clause_cte_with_column_list() {
    let select = accept("WITH cte(x, y) AS (SELECT 1, 2) SELECT * FROM cte");
    let with_clause = select.with_clause.as_ref().expect("expected a WITH clause");
    assert_eq!(
        with_clause.ctes[0].columns,
        Some(vec!["x".to_string(), "y".to_string()])
    );
}

/// The CTE's own body is a full `Select`, and the outer query can
/// reference the CTE name in its FROM clause (parsing only — codegen
/// resolution of the CTE name is #376's scope, not this ticket's).
#[test]
fn test_with_clause_cte_referenced_in_from() {
    let select = accept("WITH cte AS (SELECT id FROM t WHERE id > 0) SELECT * FROM cte");
    let with_clause = select.with_clause.as_ref().expect("expected a WITH clause");
    assert_eq!(with_clause.ctes[0].query.columns.len(), 1);
    assert!(with_clause.ctes[0].query.where_clause.is_some());
    let from = select.from.as_ref().expect("expected a FROM clause");
    assert!(matches!(&from.first.kind, TableRefKind::Name(name) if name == "cte"));
}

/// `WITH RECURSIVE` is grammatically distinct and out of scope here —
/// only the non-recursive form is implemented.
#[test]
fn test_with_recursive_is_unsupported() {
    let msg = unsupported("WITH RECURSIVE cte AS (SELECT 1) SELECT * FROM cte");
    assert!(msg.contains("RECURSIVE"), "message: {msg}");
}

/// `[NOT] MATERIALIZED` (SQLite 3.35+ CTE query-planner hint) is
/// recognized syntax the parser doesn't act on yet — pinned as
/// `Unsupported`, not `Invalid`, per the extracted-corpus regression
/// this PR fixed (`tests/corpus/extracted_sql_test.rs`).
#[test]
fn test_with_materialized_hint_is_unsupported() {
    let msg = unsupported("WITH cte AS MATERIALIZED (SELECT 1) SELECT * FROM cte");
    assert!(msg.contains("MATERIALIZED"), "message: {msg}");
}

#[test]
fn test_with_not_materialized_hint_is_unsupported() {
    let msg = unsupported("WITH cte AS NOT MATERIALIZED (SELECT 1) SELECT * FROM cte");
    assert!(msg.contains("MATERIALIZED"), "message: {msg}");
}

/// A `WITH` clause feeding `INSERT`/`UPDATE`/`DELETE` instead of
/// `SELECT` (a CTE-backed data-modifying statement) is recognized SQL
/// this grammar slice doesn't parse — pinned as `Unsupported`, not
/// `Invalid`, per the same extracted-corpus regression.
#[test]
fn test_with_clause_feeding_insert_is_unsupported() {
    let msg = unsupported("WITH cte AS (SELECT 1) INSERT INTO t SELECT * FROM cte");
    assert!(msg.contains("INSERT"), "message: {msg}");
}

/// A single-quoted string literal used as a `SELECT`-list alias is a
/// legacy SQLite compatibility quirk it accepts — recognized syntax,
/// not malformed SQL, so `AS '...'` must be `Unsupported`, not
/// `Invalid`.
#[test]
fn test_quoted_string_alias_is_unsupported() {
    let msg = unsupported("SELECT 1 AS 'x'");
    assert!(msg.contains("alias"), "message: {msg}");
}

/// Printer roundtrip: WITH-prefixed SELECT reparses to the same AST
/// (spec 002-parser Requirement 3).
#[test]
fn test_with_clause_printer_roundtrip() {
    let select1 = accept("WITH cte(x) AS (SELECT 1) SELECT * FROM cte");
    let printed1 = select1.to_string();
    let select2 = accept(&printed1);
    let printed2 = select2.to_string();
    assert_eq!(printed1, printed2);
}
