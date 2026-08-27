// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! `FROM`-subquery flattening (#566): merges a simple `FROM`-subquery
//! directly into the enclosing query instead of materializing it, so
//! base-table indexes stay visible to the planner. Runs once, right
//! after [`super::expand_views`]/[`super::expand_with_clause`] and
//! *before* [`super::push_down_where_predicates`] — flattening a
//! subquery away removes the need to push a predicate into it at all.
//! Any subquery flattening declines to touch is left for the pushdown
//! pass to still optimize.
//!
//! Eligibility mirrors sqlite3's own `flattenSubquery()` conditions
//! (`select.c`), restricted to the common case named by #566:
//!
//! - the subquery's own `FROM` is exactly one real table (`Name`, not
//!   another subquery or a `JOIN`) — a stricter shape than pushdown's
//!   `subquery_pushdown_safe` needs, since flattening leaves no scope
//!   behind to hold a nested subquery/join;
//! - no `DISTINCT`, aggregate, `GROUP BY`, `HAVING`, `LIMIT`, or
//!   compound (`UNION`) body in the subquery — any of those would
//!   change which/how many rows survive once its scope is gone;
//! - the subquery's projection is a bare `SELECT *` or a list of plain
//!   (optionally aliased) column references, so every reference to its
//!   output can be rewritten to the underlying base-table column
//!   ([`super::pushdown`]'s `ColumnMap`, reused verbatim);
//! - the enclosing `JOIN` (if any) against this table is `INNER`/plain/
//!   `CROSS`, never `LEFT`/`RIGHT`/`FULL` — flattening would otherwise
//!   change `NULL`-extension semantics;
//! - every reference to the subquery's alias anywhere in the enclosing
//!   query (`WHERE`, result columns, `GROUP BY`, `HAVING`, `ORDER BY`)
//!   is a plain column reference this pass can rewrite; a reference
//!   *inside* a nested subquery expression (`Subquery`/`Exists`/
//!   `InSubquery`/`InSubqueryMulti`) aborts flattening for this table
//!   entirely rather than risk rewriting into the wrong scope.
//!
//! On success, the outer `TableRefKind::Subquery` becomes the inner
//! query's own `TableRefKind::Name`, the inner `WHERE` is `AND`ed onto
//! the outer `WHERE` (its own qualifier, if any, stripped since the
//! flattened table now takes the outer alias), and every rewritable
//! reference to the subquery's alias elsewhere in the outer query is
//! replaced by its underlying column expression.

use super::pushdown::{subquery_column_map, ColumnMap};
use crate::codegen::select::select_has_aggregate;
use crate::parser::ast::{
    BinaryOp, Expr, ExprKind, FunctionArgs, Join, JoinConstraint, JoinOp, ResultColumn, Select,
    TableRef, TableRefKind,
};

/// Recursively flattens every eligible `FROM`-subquery in `select` into
/// its enclosing query, then descends into whatever subqueries remain
/// (either genuinely ineligible, or newly exposed one level up) so
/// flattening chains through nested views/CTEs.
pub fn flatten_from_subqueries(select: &mut Select) {
    while try_flatten_one(select) {}
    recurse_into_from_subqueries(select);
}

#[derive(Clone, Copy)]
enum TableRefSlot {
    First,
    Join(usize),
}

fn try_flatten_one(select: &mut Select) -> bool {
    let Some(num_joins) = select.from.as_ref().map(|f| f.joins.len()) else {
        return false;
    };
    if try_flatten_table_ref_at(select, TableRefSlot::First) {
        return true;
    }
    for i in 0..num_joins {
        let Some(op) = select
            .from
            .as_ref()
            .and_then(|f| f.joins.get(i))
            .map(|j| j.op)
        else {
            continue;
        };
        if !matches!(op, JoinOp::Inner | JoinOp::Cross) {
            continue;
        }
        if try_flatten_table_ref_at(select, TableRefSlot::Join(i)) {
            return true;
        }
    }
    false
}

fn table_ref_at(from: &crate::parser::ast::FromClause, slot: TableRefSlot) -> Option<&TableRef> {
    match slot {
        TableRefSlot::First => Some(&from.first),
        TableRefSlot::Join(i) => from.joins.get(i).map(|j| &j.table),
    }
}

fn try_flatten_table_ref_at(select: &mut Select, slot: TableRefSlot) -> bool {
    let require_qualified = select
        .from
        .as_ref()
        .is_none_or(|from| !from.joins.is_empty());

    let Some(from) = select.from.as_ref() else {
        return false;
    };
    let Some(table_ref) = table_ref_at(from, slot) else {
        return false;
    };
    let TableRefKind::Subquery(inner) = &table_ref.kind else {
        return false;
    };
    if !subquery_flatten_safe(inner) {
        return false;
    }
    let Some(column_map) = subquery_column_map(inner) else {
        return false;
    };
    let Some(alias) = table_ref.alias.clone() else {
        return false;
    };
    let Some(inner_from) = inner.from.as_ref() else {
        return false;
    };
    let TableRefKind::Name(inner_table_name) = &inner_from.first.kind else {
        return false;
    };
    let inner_table_name = inner_table_name.clone();
    let inner_where = inner.where_clause.clone();

    // Every reference to `alias` outside this table's own FROM slot
    // must be provably rewritable (or provably absent) before any
    // mutation happens — an all-or-nothing check, since a partial
    // rewrite would corrupt the query on failure.
    if !rewrite_alias_in_select(select, &alias, &column_map, require_qualified) {
        return false;
    }

    if let Some(mut pred) = inner_where {
        qualify_with_outer_alias(&mut pred, require_qualified.then_some(alias.as_str()));
        select.where_clause = Some(and_exprs(select.where_clause.take(), pred));
    }

    let Some(from) = select.from.as_mut() else {
        return false;
    };
    let target = match slot {
        TableRefSlot::First => Some(&mut from.first),
        TableRefSlot::Join(i) => from.joins.get_mut(i).map(|j| &mut j.table),
    };
    let Some(target) = target else {
        return false;
    };
    let span = target.span;
    *target = TableRef {
        kind: TableRefKind::Name(inner_table_name),
        alias: Some(alias),
        span,
    };
    true
}

/// Whether `inner`'s own shape is simple enough to flatten away
/// entirely: a single real base table in `FROM` (no `JOIN`, no nested
/// subquery), and none of `DISTINCT`/aggregate/`GROUP BY`/`HAVING`/
/// `LIMIT`/compound — the same rows-changing operations
/// [`super::pushdown::subquery_pushdown_safe`] rules out, plus #566's
/// extra "single real table" requirement (pushdown tolerates the
/// subquery's own `FROM` being anything with no `JOIN`; flattening
/// needs it to be a `Name` specifically, since there is no scope left
/// afterward to hold a nested subquery).
fn subquery_flatten_safe(inner: &Select) -> bool {
    if inner.distinct.is_some()
        || inner.having.is_some()
        || !inner.group_by.is_empty()
        || inner.limit.is_some()
        || !inner.compound.is_empty()
        || select_has_aggregate(inner)
    {
        return false;
    }
    let Some(from) = &inner.from else {
        return false;
    };
    from.joins.is_empty() && matches!(from.first.kind, TableRefKind::Name(_))
}

/// Rewrites every reference to `alias` across `select`'s own `WHERE`,
/// result columns, `GROUP BY`, `HAVING`, and `ORDER BY` — everywhere
/// *except* the `FROM` clause itself, which the caller replaces
/// separately. Returns `false`, leaving `select` unmodified, if any
/// such reference can't be proven safe to rewrite (an unmapped column,
/// or a reference nested inside a subquery expression).
fn rewrite_alias_in_select(
    select: &mut Select,
    alias: &str,
    column_map: &ColumnMap,
    require_qualified: bool,
) -> bool {
    let mut candidate = select.clone();
    let ok = (|| {
        for col in &mut candidate.columns {
            match col {
                ResultColumn::Star => {}
                ResultColumn::TableStar { table } => {
                    // `alias.*` has no single underlying expression to
                    // rewrite to — bail rather than guess.
                    if table.eq_ignore_ascii_case(alias) {
                        return false;
                    }
                }
                ResultColumn::Expr { expr, .. } => {
                    if !rewrite_expr(expr, alias, column_map, require_qualified) {
                        return false;
                    }
                }
            }
        }
        if let Some(w) = &mut candidate.where_clause {
            if !rewrite_expr(w, alias, column_map, require_qualified) {
                return false;
            }
        }
        for e in &mut candidate.group_by {
            if !rewrite_expr(e, alias, column_map, require_qualified) {
                return false;
            }
        }
        if let Some(h) = &mut candidate.having {
            if !rewrite_expr(h, alias, column_map, require_qualified) {
                return false;
            }
        }
        for term in &mut candidate.order_by {
            if !rewrite_expr(&mut term.expr, alias, column_map, require_qualified) {
                return false;
            }
        }
        // A sibling JOIN's own ON-condition can also reference this
        // alias (e.g. joining a third table against it).
        if let Some(from) = &mut candidate.from {
            for join in &mut from.joins {
                if let Some(JoinConstraint::On(cond)) = &mut join.constraint {
                    if !rewrite_expr(cond, alias, column_map, require_qualified) {
                        return false;
                    }
                }
            }
        }
        true
    })();
    if ok {
        *select = candidate;
    }
    ok
}

/// Rewrites a single expression in place, replacing every column
/// reference against `alias` with its underlying expression per
/// `column_map`. Returns `false` (caller discards the partially
/// mutated clone) as soon as a column can't be resolved, or a nested
/// subquery expression is found to reference `alias` at all (rewriting
/// into a different scope is out of scope for this pass).
fn rewrite_expr(
    expr: &mut Expr,
    alias: &str,
    column_map: &ColumnMap,
    require_qualified: bool,
) -> bool {
    match &mut expr.kind {
        ExprKind::Literal(_) | ExprKind::Param(_) => true,
        ExprKind::Column {
            table,
            catalog,
            name,
        } => {
            if catalog.is_some() {
                return true;
            }
            let qualifies = match table {
                Some(t) => t.eq_ignore_ascii_case(alias),
                None => !require_qualified,
            };
            if !qualifies {
                return true;
            }
            match column_map {
                ColumnMap::Wildcard => {
                    *table = None;
                    true
                }
                ColumnMap::Explicit(cols) => {
                    match cols.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
                        // `underlying` is always a bare `Column` (enforced by
                        // `subquery_column_map`), carrying no qualifier or
                        // one matching the subquery's own single table — a
                        // fine reference *inside* that subquery's unambiguous
                        // scope. The flattened table now lives in the outer
                        // scope under `alias`: when a sibling join is present,
                        // re-qualify explicitly so the result can't become
                        // ambiguous against a same-named column elsewhere in
                        // the outer FROM. When there is no sibling join,
                        // leave it unqualified instead — `Scope::single`
                        // (the codegen path a from-less-join query takes)
                        // has no notion of the FROM clause's own alias, so a
                        // qualified reference against it fails to resolve
                        // even outside this pass — a pre-existing gap
                        // unrelated to flattening (confirmed against
                        // `SELECT * FROM t AS x WHERE x.col > 1` on main).
                        Some((_, underlying)) => {
                            let ExprKind::Column { name: col_name, .. } = &underlying.kind else {
                                return false;
                            };
                            expr.kind = ExprKind::Column {
                                table: require_qualified.then(|| alias.to_string()),
                                catalog: None,
                                name: col_name.clone(),
                            };
                            true
                        }
                        None => false,
                    }
                }
            }
        }
        ExprKind::FunctionCall { args, .. } => match args {
            FunctionArgs::Star => true,
            FunctionArgs::List(list) => list
                .iter_mut()
                .all(|e| rewrite_expr(e, alias, column_map, require_qualified)),
        },
        ExprKind::Unary { expr: e, .. }
        | ExprKind::IsNull { expr: e, .. }
        | ExprKind::Cast { expr: e, .. }
        | ExprKind::Collate { expr: e, .. }
        | ExprKind::Paren(e) => rewrite_expr(e, alias, column_map, require_qualified),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Is { lhs, rhs, .. } => {
            rewrite_expr(lhs, alias, column_map, require_qualified)
                && rewrite_expr(rhs, alias, column_map, require_qualified)
        }
        ExprKind::Between {
            expr: e, lo, hi, ..
        } => {
            rewrite_expr(e, alias, column_map, require_qualified)
                && rewrite_expr(lo, alias, column_map, require_qualified)
                && rewrite_expr(hi, alias, column_map, require_qualified)
        }
        ExprKind::In { expr: e, list, .. } => {
            rewrite_expr(e, alias, column_map, require_qualified)
                && list
                    .iter_mut()
                    .all(|item| rewrite_expr(item, alias, column_map, require_qualified))
        }
        ExprKind::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            rewrite_expr(e, alias, column_map, require_qualified)
                && rewrite_expr(pattern, alias, column_map, require_qualified)
                && match escape {
                    Some(esc) => rewrite_expr(esc, alias, column_map, require_qualified),
                    None => true,
                }
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            let operand_ok = match operand {
                Some(o) => rewrite_expr(o, alias, column_map, require_qualified),
                None => true,
            };
            let whens_ok = whens.iter_mut().all(|(w, t)| {
                rewrite_expr(w, alias, column_map, require_qualified)
                    && rewrite_expr(t, alias, column_map, require_qualified)
            });
            let else_ok = match else_ {
                Some(e) => rewrite_expr(e, alias, column_map, require_qualified),
                None => true,
            };
            operand_ok && whens_ok && else_ok
        }
        ExprKind::Subquery(inner)
        | ExprKind::Exists {
            subquery: inner, ..
        } => !select_references_alias(inner, alias),
        ExprKind::InSubquery {
            expr: e, subquery, ..
        } => {
            rewrite_expr(e, alias, column_map, require_qualified)
                && !select_references_alias(subquery, alias)
        }
        ExprKind::InSubqueryMulti {
            exprs, subquery, ..
        } => {
            exprs
                .iter_mut()
                .all(|e| rewrite_expr(e, alias, column_map, require_qualified))
                && !select_references_alias(subquery, alias)
        }
    }
}

/// Conservative detector for whether `select` (a nested subquery
/// expression's body) references `alias` anywhere at all, at any
/// depth — used to veto rewriting into a scope this pass doesn't
/// attempt to reason about, rather than trying to distinguish a
/// genuinely correlated reference from a same-named but unrelated one.
fn select_references_alias(select: &Select, alias: &str) -> bool {
    select.columns.iter().any(|c| match c {
        ResultColumn::Star => false,
        ResultColumn::TableStar { table } => table.eq_ignore_ascii_case(alias),
        ResultColumn::Expr { expr, .. } => expr_references_alias(expr, alias),
    }) || select
        .where_clause
        .as_ref()
        .is_some_and(|e| expr_references_alias(e, alias))
        || select
            .group_by
            .iter()
            .any(|e| expr_references_alias(e, alias))
        || select
            .having
            .as_ref()
            .is_some_and(|e| expr_references_alias(e, alias))
        || select
            .order_by
            .iter()
            .any(|t| expr_references_alias(&t.expr, alias))
        || select.from.as_ref().is_some_and(|from| {
            table_ref_references_alias(&from.first, alias)
                || from.joins.iter().any(|j| join_references_alias(j, alias))
        })
}

fn table_ref_references_alias(table_ref: &TableRef, alias: &str) -> bool {
    matches!(&table_ref.kind, TableRefKind::Subquery(inner) if select_references_alias(inner, alias))
}

fn join_references_alias(join: &Join, alias: &str) -> bool {
    table_ref_references_alias(&join.table, alias)
        || matches!(&join.constraint, Some(JoinConstraint::On(e)) if expr_references_alias(e, alias))
}

fn expr_references_alias(expr: &Expr, alias: &str) -> bool {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Param(_) => false,
        ExprKind::Column { table, .. } => table
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case(alias)),
        ExprKind::FunctionCall { args, .. } => match args {
            FunctionArgs::Star => false,
            FunctionArgs::List(list) => list.iter().any(|e| expr_references_alias(e, alias)),
        },
        ExprKind::Unary { expr: e, .. }
        | ExprKind::IsNull { expr: e, .. }
        | ExprKind::Cast { expr: e, .. }
        | ExprKind::Collate { expr: e, .. }
        | ExprKind::Paren(e) => expr_references_alias(e, alias),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Is { lhs, rhs, .. } => {
            expr_references_alias(lhs, alias) || expr_references_alias(rhs, alias)
        }
        ExprKind::Between {
            expr: e, lo, hi, ..
        } => {
            expr_references_alias(e, alias)
                || expr_references_alias(lo, alias)
                || expr_references_alias(hi, alias)
        }
        ExprKind::In { expr: e, list, .. } => {
            expr_references_alias(e, alias) || list.iter().any(|i| expr_references_alias(i, alias))
        }
        ExprKind::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_references_alias(e, alias)
                || expr_references_alias(pattern, alias)
                || escape
                    .as_deref()
                    .is_some_and(|esc| expr_references_alias(esc, alias))
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            operand
                .as_deref()
                .is_some_and(|o| expr_references_alias(o, alias))
                || whens.iter().any(|(w, t)| {
                    expr_references_alias(w, alias) || expr_references_alias(t, alias)
                })
                || else_
                    .as_deref()
                    .is_some_and(|e| expr_references_alias(e, alias))
        }
        ExprKind::Subquery(inner)
        | ExprKind::Exists {
            subquery: inner, ..
        } => select_references_alias(inner, alias),
        ExprKind::InSubquery {
            expr: e, subquery, ..
        } => expr_references_alias(e, alias) || select_references_alias(subquery, alias),
        ExprKind::InSubqueryMulti {
            exprs, subquery, ..
        } => {
            exprs.iter().any(|e| expr_references_alias(e, alias))
                || select_references_alias(subquery, alias)
        }
    }
}

/// Rewrites every column reference in `expr`'s qualifier to `alias` —
/// the subquery's own `WHERE` was written against its single base
/// table, so every column in it belongs to that one table regardless
/// of whether the original reference happened to be qualified; once
/// flattened, that table lives in the outer scope under `alias`.
/// `alias` is `None` when there's no sibling join to disambiguate
/// against: `Scope::single` (the codegen path a from-less-join query
/// takes) has no notion of the FROM clause's own alias, so an
/// explicitly qualified reference against it fails to resolve even
/// outside this pass — a pre-existing gap unrelated to flattening
/// (confirmed against `SELECT * FROM t AS x WHERE x.col > 1` on main).
fn qualify_with_outer_alias(expr: &mut Expr, alias: Option<&str>) {
    match &mut expr.kind {
        ExprKind::Column { table, .. } => {
            *table = alias.map(str::to_string);
        }
        ExprKind::Unary { expr: e, .. }
        | ExprKind::IsNull { expr: e, .. }
        | ExprKind::Cast { expr: e, .. }
        | ExprKind::Collate { expr: e, .. }
        | ExprKind::Paren(e) => qualify_with_outer_alias(e, alias),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Is { lhs, rhs, .. } => {
            qualify_with_outer_alias(lhs, alias);
            qualify_with_outer_alias(rhs, alias);
        }
        ExprKind::Between {
            expr: e, lo, hi, ..
        } => {
            qualify_with_outer_alias(e, alias);
            qualify_with_outer_alias(lo, alias);
            qualify_with_outer_alias(hi, alias);
        }
        ExprKind::In { expr: e, list, .. } => {
            qualify_with_outer_alias(e, alias);
            for item in list {
                qualify_with_outer_alias(item, alias);
            }
        }
        ExprKind::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            qualify_with_outer_alias(e, alias);
            qualify_with_outer_alias(pattern, alias);
            if let Some(esc) = escape {
                qualify_with_outer_alias(esc, alias);
            }
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            if let Some(o) = operand {
                qualify_with_outer_alias(o, alias);
            }
            for (w, t) in whens {
                qualify_with_outer_alias(w, alias);
                qualify_with_outer_alias(t, alias);
            }
            if let Some(e) = else_ {
                qualify_with_outer_alias(e, alias);
            }
        }
        ExprKind::FunctionCall { args, .. } => {
            if let FunctionArgs::List(list) = args {
                for e in list {
                    qualify_with_outer_alias(e, alias);
                }
            }
        }
        ExprKind::Literal(_)
        | ExprKind::Param(_)
        | ExprKind::Subquery(_)
        | ExprKind::Exists { .. }
        | ExprKind::InSubquery { .. }
        | ExprKind::InSubqueryMulti { .. } => {}
    }
}

fn and_exprs(existing: Option<Expr>, addition: Expr) -> Expr {
    match existing {
        Some(e) => Expr {
            span: e.span,
            kind: ExprKind::Binary {
                op: BinaryOp::And,
                lhs: Box::new(e),
                rhs: Box::new(addition),
            },
        },
        None => addition,
    }
}

fn recurse_into_from_subqueries(select: &mut Select) {
    let Some(from) = &mut select.from else {
        return;
    };
    if let TableRefKind::Subquery(inner) = &mut from.first.kind {
        flatten_from_subqueries(inner);
    }
    for join in &mut from.joins {
        if let TableRefKind::Subquery(inner) = &mut join.table.kind {
            flatten_from_subqueries(inner);
        }
    }
}
