use super::join_access::{choose_join_access, JoinAccess};
use super::limit_scan::{find_covering_index, is_rowid_reference, top_level_equality_operands};
use super::*;
/// One row of `EXPLAIN QUERY PLAN` output (#243) -- SQLite's own EQP
/// shape (`id, parent, notused, detail`), distinct from plain
/// `EXPLAIN`'s per-instruction [`crate::vdbe::explain::ExplainRow`].
/// `detail` reads like the oracle's own EQP (`SCAN ...`/`SEARCH ...
/// USING ...`) but isn't guaranteed byte-identical -- Requirement 10's
/// VM-diff guarantee is plain `EXPLAIN`'s job, not this human-readable
/// summary's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EqpRow {
    /// This row's identifier within the plan.
    pub id: i32,
    /// The `id` of this row's parent in the plan tree (0 for a top-level row).
    pub parent: i32,
    /// Unused column, kept for SQLite's `EXPLAIN QUERY PLAN` shape.
    pub notused: i32,
    /// Human-readable plan step, e.g. `SCAN ...`/`SEARCH ... USING ...`.
    pub detail: String,
}

/// A table binding's `FROM`-clause display name for EQP output:
/// `name AS alias` when aliased, `name` otherwise -- matching how a
/// `Column` reference would need to qualify it.
pub(super) fn eqp_display_name(table_ref: &TableRef) -> String {
    let name = table_ref.name().unwrap_or("(subquery)");
    match &table_ref.alias {
        Some(alias) => format!("{name} AS {alias}"),
        None => name.to_string(),
    }
}

/// The identifier a [`TableBinding`] tracks alongside its `alias` —
/// a subquery-in-FROM (#257) has no catalog name of its own, so this
/// falls back to its (mandatory) alias.
pub(super) fn table_binding_name(table_ref: &TableRef) -> String {
    table_ref
        .name()
        .map(str::to_string)
        .or_else(|| table_ref.alias.clone())
        .unwrap_or_default()
}

/// Builds `EXPLAIN QUERY PLAN`'s output for `select` (#243): one row per
/// `FROM`-clause table, `SCAN` for a full `Rewind`/`Next` scan or
/// `SEARCH ... USING ...` for a `SeekRowid`/`SeekIndexEq` point lookup --
/// reusing [`choose_join_access`] (the join codegen's own decision
/// function) for a join's inner tables, and the same rowid-equality
/// check [`try_compile_rowid_seek`] uses for a single-table `SELECT`'s
/// `WHERE` clause, so the report can never drift from what
/// [`compile_select_joined`]/[`compile_direct_scan`] actually compile.
///
/// #250 note: this reports FROM-clause order, not the RIGHT-JOIN
/// execution reordering `compile_select_joined` may apply internally --
/// `choose_join_access` is still evaluated against each table's
/// FROM-order-preceding siblings, matching what a RIGHT-JOIN-free query
/// actually executes; a query that also has a RIGHT JOIN keeps working
/// via the ordinary (unseeked) fallback below since `on_expr` there
/// won't resolve against these FROM-order-built `prior_bindings`.
///
/// #470's cost-model reordering of a pure `INNER`/`CROSS` chain *is*
/// reflected here (unlike the RIGHT-JOIN case above): `execution_order`
/// mirrors `join_order::plan_join_order`'s decision exactly, rows are
/// emitted in that order, and each join's `ON` clause is associated
/// with the execution level where every table it references is first
/// fully bound (via `join_order::referenced_binding_indices`) rather
/// than assumed adjacent -- matching `compile_select_joined`'s own
/// `LevelCheck` placement for a reordered chain. A level with more than
/// one such check (a multi-table `ON` chain landing on the same level)
/// falls back to reporting a plain `SCAN`, same as
/// `compile_join_level_traverse`'s single-check-only seek optimization.
pub fn explain_query_plan(
    select: &Select,
    schemas: &[TableSchema],
    stats_by_table: &std::collections::HashMap<String, crate::planner::Stats>,
) -> Result<Vec<EqpRow>, CodegenError> {
    let Some(from) = &select.from else {
        return Err(CodegenError::NoFromClause);
    };
    let table_refs: Vec<&TableRef> = std::iter::once(&from.first)
        .chain(from.joins.iter().map(|j| &j.table))
        .collect();
    if schemas.len() != table_refs.len() {
        return Err(CodegenError::Unsupported {
            reason: format!(
                "explain_query_plan needs one schema per FROM table ({} tables, {} schemas \
                 given)",
                table_refs.len(),
                schemas.len()
            ),
        });
    }
    let bindings: Vec<TableBinding> = table_refs
        .iter()
        .zip(schemas.iter())
        .enumerate()
        .map(|(i, (table_ref, schema))| TableBinding {
            alias: table_ref.alias.clone(),
            name: table_binding_name(table_ref),
            schema: schema.clone(),
            cursor: i32::try_from(i).unwrap_or(0),
            forced_null: false,
            stats: stats_by_table
                .get(&schema.name)
                .cloned()
                .unwrap_or_default(),
        })
        .collect();
    let n = bindings.len();

    let reorder = super::join_order::is_reorderable_inner_chain(from).then(|| {
        let costs = super::join_order::scan_costs(schemas, stats_by_table);
        super::join_order::plan_join_order(&costs)
    });
    let execution_order: Vec<usize> = reorder.clone().unwrap_or_else(|| (0..n).collect());
    let mut pos_of = vec![0usize; n];
    for (pos, &orig) in execution_order.iter().enumerate() {
        if let Some(slot) = pos_of.get_mut(orig) {
            *slot = pos;
        }
    }
    // Only populated when `reorder` fired: `level_joins[level]` lists
    // every join index whose `ON` clause is checkable once execution
    // reaches `level` (i.e. every table it references has
    // `pos_of[..] <= level`) -- mirrors `compile_select_joined_scan`'s
    // reordered-chain `LevelCheck` placement.
    let mut level_joins: Vec<Vec<usize>> = vec![Vec::new(); n];
    if reorder.is_some() {
        for (j, join) in from.joins.iter().enumerate() {
            let right_idx = j.saturating_add(1);
            let on_expr = match &join.constraint {
                Some(JoinConstraint::On(e)) => Some(e),
                _ => None,
            };
            let level = match on_expr {
                Some(e) => super::join_order::referenced_binding_indices(e, &bindings)
                    .into_iter()
                    .chain(std::iter::once(right_idx))
                    .filter_map(|i| pos_of.get(i).copied())
                    .max()
                    .unwrap_or(0),
                None => pos_of.get(right_idx).copied().unwrap_or(0),
            };
            if let Some(slot) = level_joins.get_mut(level) {
                slot.push(j);
            }
        }
    }

    let mut rows = Vec::with_capacity(n);
    for (level, &orig) in execution_order.iter().enumerate() {
        let Some(&table_ref) = table_refs.get(orig) else {
            continue;
        };
        let Some(binding) = bindings.get(orig) else {
            continue;
        };
        let on_expr = if reorder.is_some() {
            match level_joins.get(level).map(Vec::as_slice) {
                Some([j]) => from.joins.get(*j).and_then(|join| match &join.constraint {
                    Some(JoinConstraint::On(e)) => Some(e),
                    _ => None,
                }),
                _ => None,
            }
        } else {
            level
                .checked_sub(1)
                .and_then(|i| from.joins.get(i))
                .and_then(|j| j.constraint.as_ref())
                .and_then(|c| match c {
                    JoinConstraint::On(e) => Some(e),
                    JoinConstraint::Using(_) => None,
                })
        };
        let prior_bindings: Vec<TableBinding> = if reorder.is_some() {
            bindings
                .iter()
                .enumerate()
                .filter(|&(i, _)| pos_of.get(i).is_some_and(|&p| p < level))
                .map(|(_, b)| b.clone())
                .collect()
        } else {
            bindings.get(..level).unwrap_or(&[]).to_vec()
        };
        let prior_bindings = prior_bindings.as_slice();
        let access = if level == 0 {
            // The outermost table has no `ON` clause to seek against --
            // an equality `WHERE` predicate against its rowid still
            // gets `try_compile_rowid_seek`'s single-table fast path
            // (#137), so report that here too rather than a blanket
            // SCAN.
            select
                .where_clause
                .as_ref()
                .and_then(|where_expr| top_level_equality_operands(where_expr))
                .and_then(|(lhs, rhs)| {
                    if is_rowid_reference(&binding.schema, lhs) {
                        Some(JoinAccess::Rowid(rhs.clone()))
                    } else if is_rowid_reference(&binding.schema, rhs) {
                        Some(JoinAccess::Rowid(lhs.clone()))
                    } else {
                        None
                    }
                })
        } else {
            on_expr.and_then(|e| choose_join_access(binding, e, prior_bindings))
        };
        // #444: a covering-index scan only applies to the outermost
        // table's own `WHERE` clause (like the rowid-seek check above),
        // and only when `access` didn't already find a rowid seek --
        // `find_covering_index` only fires for a non-rowid indexed
        // column, so the two never actually overlap, but checking
        // `access.is_none()` keeps this branch's precedence explicit.
        let covering = if level == 0 && access.is_none() {
            find_covering_index(&binding.schema, select)
        } else {
            None
        };
        let detail = match (
            access,
            covering
                .as_ref()
                .and_then(|m| binding.schema.indexes.get(m.index_position)),
        ) {
            (_, Some(index)) => format!(
                "SEARCH {} USING COVERING INDEX {} ({}=?)",
                eqp_display_name(table_ref),
                index.name,
                index
                    .columns
                    .first()
                    .map_or_else(String::new, |c| c.name.clone())
            ),
            (None, None) => format!("SCAN {}", eqp_display_name(table_ref)),
            (Some(JoinAccess::Rowid(_)), None) => format!(
                "SEARCH {} USING INTEGER PRIMARY KEY (rowid=?)",
                eqp_display_name(table_ref)
            ),
            (Some(JoinAccess::UniqueIndex { index, .. }), None) => format!(
                "SEARCH {} USING INDEX {} ({}=?)",
                eqp_display_name(table_ref),
                index.name,
                index
                    .columns
                    .first()
                    .map_or_else(String::new, |c| c.name.clone())
            ),
        };
        rows.push(EqpRow {
            id: i32::try_from(level).unwrap_or(0),
            parent: 0,
            notused: 0,
            detail,
        });
    }
    Ok(rows)
}
