// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #470/#462 (spec 011): reorders a purely `INNER`/`CROSS` join chain's
//! *execution* order by estimated table size, using the `sqlite_stat1`
//! cost model (#461, `crate::planner`). `LEFT`/`RIGHT`/`FULL`/`NATURAL`-
//! with-dependency chains keep their original FROM-clause order
//! unconditionally (see [`plan_join_order`]'s guard) — only a chain
//! where every join is unconditionally commutative is ever reordered,
//! so this module can never change a query's result set, only the
//! order tables are scanned in.
//!
//! Without `ANALYZE` history, every table's estimated row count is
//! `u64::MAX` (`crate::planner::estimate_scan_cost`'s conservative
//! default), so the cost-sort below is a stable no-op and execution
//! order matches the pre-#470 FROM-clause order byte-for-byte — the
//! same "stats-free behavior is unaffected" guarantee #461 already
//! makes for `join_access::choose_join_access`.

use super::limit_scan::{is_rowid_reference, top_level_equality_operands};
use super::*;
use crate::planner::{estimate_scan_cost, Stats};

/// Whether every join in `from.joins` is a plain `INNER`/`CROSS` join
/// (including already-resolved `NATURAL`/`USING`, which lower to
/// `JoinOp::Inner` with a synthesized `ON`) — the only shape safe to
/// reorder, since `LEFT`/`RIGHT`/`FULL` all encode a result-set-visible
/// asymmetry between their two sides that reordering would break.
pub(super) fn is_reorderable_inner_chain(from: &FromClause) -> bool {
    from.joins
        .iter()
        .all(|j| matches!(j.op, JoinOp::Inner | JoinOp::Cross))
}

/// Picks the execution order for a reorderable inner/cross join chain:
/// original FROM-clause indices `0..n`, sorted ascending by estimated
/// row count (ties broken by original position — Rust's sort is
/// stable), so the smallest table is scanned outermost. `costs[i]` is
/// table `i`'s own `estimate_scan_cost(&stats).estimated_rows`.
pub(super) fn plan_join_order(costs: &[u64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..costs.len()).collect();
    order.sort_by_key(|&i| costs.get(i).copied().unwrap_or(u64::MAX));
    order
}

/// [`plan_join_order`]'s per-table cost input: each schema's own
/// unconditional full-scan row estimate, looked up by name in
/// `stats_by_table` (missing entries default to [`Stats::default`],
/// i.e. the conservative "no `ANALYZE` history" estimate) — except a
/// table [`seekable_tables`] marks as reachable via a rowid/unique-index
/// point lookup off some other table in the chain, which is given
/// `u64::MAX` regardless of its own row count so [`plan_join_order`]'s
/// ascending sort always places it last (innermost), letting
/// `join_access::choose_join_access` turn its `ON` equality into a
/// `SeekRowid`/`SeekIndexEq` instead of a full scan (#510). A table's
/// own raw size is irrelevant to this choice: a seek is O(1)/O(log n)
/// regardless, so it is always cheaper as the inner probe than as the
/// outer scan, mirroring `choose_join_access`'s own unconditional rowid
/// preference.
pub(super) fn scan_costs(
    schemas: &[TableSchema],
    stats_by_table: &std::collections::HashMap<String, Stats>,
    seekable: &[bool],
) -> Vec<u64> {
    schemas
        .iter()
        .zip(seekable)
        .map(|(schema, &is_seekable)| {
            if is_seekable {
                return u64::MAX;
            }
            let stats = stats_by_table
                .get(&schema.name)
                .cloned()
                .unwrap_or_default();
            estimate_scan_cost(&stats).estimated_rows
        })
        .collect()
}

/// Whether `expr` (a join's resolved `ON` constraint) is a single
/// top-level equality between `schema`'s own rowid alias or a
/// single-column `UNIQUE` index and anything on the other side — the
/// same structural shape `join_access::choose_join_access` looks for,
/// checked here *before* execution order is fixed (so, unlike
/// `choose_join_access`, it can't yet confirm the other side only
/// references already-bound tables — [`seekable_tables`]'s caller only
/// uses this to bias ordering, and `choose_join_access` re-validates the
/// real safety condition once order is fixed, so an overly-optimistic
/// guess here only costs a suboptimal order, never a correctness bug).
fn is_seekable_equality(schema: &TableSchema, expr: &Expr) -> bool {
    let Some((lhs, rhs)) = top_level_equality_operands(expr) else {
        return false;
    };
    [(lhs, rhs), (rhs, lhs)].into_iter().any(|(this_side, _)| {
        let ExprKind::Column { name, .. } = &this_side.kind else {
            return false;
        };
        if column_index(schema, name).is_none() {
            return false;
        }
        if is_rowid_reference(schema, this_side) {
            return true;
        }
        schema.indexes.iter().any(|idx| {
            idx.unique
                && idx.columns.len() == 1
                && idx
                    .columns
                    .first()
                    .is_some_and(|c| c.name.eq_ignore_ascii_case(name))
        })
    })
}

/// Per original FROM-clause index, whether that table can be reached via
/// a rowid/unique-index point lookup off the join that brings it in
/// (`constraints[i - 1]`, `resolve_join_constraint`'s output for the
/// join whose right-hand table is index `i`) — table `0` is never
/// seekable since it has no incoming join. Used to bias
/// [`scan_costs`]/[`plan_join_order`] toward placing such a table
/// innermost regardless of its own size (#510).
pub(super) fn seekable_tables(schemas: &[TableSchema], constraints: &[Option<Expr>]) -> Vec<bool> {
    let mut seekable = vec![false; schemas.len()];
    for (join_idx, constraint) in constraints.iter().enumerate() {
        let right_idx = join_idx.saturating_add(1);
        let (Some(expr), Some(schema)) = (constraint, schemas.get(right_idx)) else {
            continue;
        };
        if let Some(slot) = seekable.get_mut(right_idx) {
            *slot = is_seekable_equality(schema, expr);
        }
    }
    seekable
}

/// Collects the original FROM-clause indices `expr` references against
/// `bindings` (in original, not execution, order) — used to place a
/// reordered chain's join constraint at the first execution level where
/// every table it reads is already bound (see
/// [`super::joins::compile_select_joined_scan`]'s use of this for the
/// reordered path). A qualified `Column` matches exactly the one
/// binding its qualifier names; an unqualified one matches every
/// binding whose schema has that column name (ambiguous in general, but
/// safe here — including an extra candidate can only push the
/// constraint's level later, never earlier, so it never under-counts a
/// real dependency). Any expression shape not explicitly walked
/// (a scalar subquery, `EXISTS`, ...) conservatively reports every
/// binding as referenced rather than risk missing one.
pub(super) fn referenced_binding_indices(expr: &Expr, bindings: &[TableBinding]) -> Vec<usize> {
    let mut acc = Vec::new();
    collect_referenced_binding_indices(expr, bindings, &mut acc);
    acc.sort_unstable();
    acc.dedup();
    acc
}

fn collect_referenced_binding_indices(
    expr: &Expr,
    bindings: &[TableBinding],
    acc: &mut Vec<usize>,
) {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Param(_) => {}
        ExprKind::Column { table, name, .. } => {
            for (i, binding) in bindings.iter().enumerate() {
                let matches = match table {
                    Some(t) => binding.matches_qualifier(t),
                    None => column_index(&binding.schema, name).is_some(),
                };
                if matches {
                    acc.push(i);
                }
            }
        }
        ExprKind::FunctionCall { args, .. } => {
            if let FunctionArgs::List(list) = args {
                for e in list {
                    collect_referenced_binding_indices(e, bindings, acc);
                }
            }
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::IsNull { expr, .. }
        | ExprKind::Cast { expr, .. }
        | ExprKind::Collate { expr, .. }
        | ExprKind::Paren(expr) => collect_referenced_binding_indices(expr, bindings, acc),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Is { lhs, rhs, .. } => {
            collect_referenced_binding_indices(lhs, bindings, acc);
            collect_referenced_binding_indices(rhs, bindings, acc);
        }
        ExprKind::Between { expr, lo, hi, .. } => {
            collect_referenced_binding_indices(expr, bindings, acc);
            collect_referenced_binding_indices(lo, bindings, acc);
            collect_referenced_binding_indices(hi, bindings, acc);
        }
        ExprKind::In { expr, list, .. } => {
            collect_referenced_binding_indices(expr, bindings, acc);
            for e in list {
                collect_referenced_binding_indices(e, bindings, acc);
            }
        }
        ExprKind::Like {
            expr,
            pattern,
            escape,
            ..
        } => {
            collect_referenced_binding_indices(expr, bindings, acc);
            collect_referenced_binding_indices(pattern, bindings, acc);
            if let Some(escape) = escape {
                collect_referenced_binding_indices(escape, bindings, acc);
            }
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            if let Some(operand) = operand {
                collect_referenced_binding_indices(operand, bindings, acc);
            }
            for (when, then) in whens {
                collect_referenced_binding_indices(when, bindings, acc);
                collect_referenced_binding_indices(then, bindings, acc);
            }
            if let Some(else_) = else_ {
                collect_referenced_binding_indices(else_, bindings, acc);
            }
        }
        // Scalar subquery / EXISTS / IN (SELECT ...): conservatively
        // treat as referencing every table rather than walk into a
        // nested `Select` (which may itself be correlated against any
        // binding in scope).
        ExprKind::Subquery(_)
        | ExprKind::Exists { .. }
        | ExprKind::InSubquery { .. }
        | ExprKind::InSubqueryMulti { .. } => {
            acc.extend(0..bindings.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_join_order_sorts_by_cost_ascending() {
        let costs = vec![100, 5, 50];
        assert_eq!(plan_join_order(&costs), vec![1, 2, 0]);
    }

    #[test]
    fn plan_join_order_is_stable_on_ties() {
        // Every table u64::MAX (no ANALYZE) -> original order preserved.
        let costs = vec![u64::MAX, u64::MAX, u64::MAX];
        assert_eq!(plan_join_order(&costs), vec![0, 1, 2]);
    }
}
