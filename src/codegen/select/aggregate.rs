use super::limit_scan::{compile_limit_setup, emit_limit_guard, emit_offset_guard, LimitState};
use super::order_by::{order_by_target_for_expr, OrderByPlan, OrderByTarget};
use super::projection::{compile_row_values, result_columns, ResultColumnPlan};
use super::*;
/// #239: `GROUP BY` / `HAVING`. Strategy mirrors real SQLite's
/// sort-then-group `select.c` shape rather than a hash table, since the
/// `Sorter*` opcode family this compiler already has for `ORDER BY`
/// (see [`compile_sorted_scan`]) does the heavy lifting for free: pass 1
/// sorts every WHERE-matching row by its GROUP BY key, pass 2 walks the
/// sorted stream detecting key changes as group boundaries, accumulating
/// one register (or two, for `avg`) per aggregate call, and flushing a
/// finalized output row through `sink` at each boundary (and once more
/// after the loop, for the final group).
///
/// Known simplifications (documented rather than silently wrong):
/// - `GROUP BY`/`HAVING` combined with `ORDER BY` or `DISTINCT` on the
///   same `SELECT` are rejected outright (see the caller) rather than
///   composed.
/// - Only `count`/`sum`/`avg`/`min`/`max` are supported aggregates;
///   `group_concat`/`string_agg`/`total` are rejected.
/// - Aggregate-call detection only descends through `Paren`/`Collate`/
///   `Unary`/`Binary` wrappers — an aggregate nested inside `CASE`/
///   `BETWEEN`/`IN`/`LIKE` is not found, and compiling it falls through
///   to `compile_value`'s ordinary aggregate-rejection error.
/// - A `GROUP BY`/aggregate-argument expression that itself reads the
///   table's `INTEGER PRIMARY KEY` rowid-alias column mid-expression
///   (not as a bare column) reads the wrong value against the pass-2
///   pseudo cursor — narrow enough (grouping/aggregating by a *bare*
///   rowid-alias column is handled correctly; only a compound
///   expression referencing it is affected) not to block this ticket.
///
/// `select.group_by.is_empty()` is also the entry point for #287's
/// implicit whole-table group: with no `GROUP BY` key at all, every
/// WHERE-matching row belongs to one synthetic group (`group_targets`
/// is empty, so the boundary check below only ever fires on the very
/// first row). The one place that still needs to differ from an
/// explicit `GROUP BY ()`-shaped zero-row result is the *tail* flush:
/// an explicit `GROUP BY` over zero matching rows correctly produces
/// zero groups, but a whole-table aggregate with zero matching rows
/// still produces exactly one row (`count(*) = 0`, other aggregates
/// `NULL`) — `implicit_group` selects that behavior.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub(super) fn compile_grouped_scan<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    implicit_group: bool,
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    let table_scope = Scope::single(schema, cursors.table).with_catalog(catalog.to_vec());
    let pseudo_scope = Scope::single(schema, cursors.pseudo).with_catalog(catalog.to_vec());
    let group_targets: Vec<OrderByTarget> = select
        .group_by
        .iter()
        .map(|expr| order_by_target_for_expr(expr, schema))
        .collect::<Result<_, _>>()?;

    // Pass 1: buffer every WHERE-matching row's full column tuple, plus
    // a trailing register per computed (non-bare-column) GROUP BY
    // expression, sorted by the GROUP BY key — identical in shape to
    // `compile_sorted_scan`'s ORDER BY pass 1.
    let sorter_open_addr = em.emit(Instruction::with_p4(
        Opcode::SorterOpen,
        cursors.sort,
        0,
        0,
        P4::None,
    ));

    let scan_rewind = em.emit(Instruction::new(Opcode::Rewind, cursors.table, 0, 0));
    let sort_step = em.new_label();
    em.patch_p2(scan_rewind, sort_step);
    let scan_loop = em.new_label();
    em.place(scan_loop);

    let scan_skip = em.new_label();
    if let Some(where_expr) = &select.where_clause {
        compile_cond(
            em,
            reg,
            &table_scope,
            where_expr,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(scan_skip)),
        )?;
    }
    let (first, _schema_count) = compile_row_values(
        em,
        reg,
        schema,
        &schema
            .columns
            .iter()
            .map(|c| ResultColumnPlan::Column(c.clone()))
            .collect::<Vec<_>>(),
        cursors.table,
        false,
        catalog,
    )?;

    let mut sort_keys = Vec::with_capacity(group_targets.len());
    for (expr, target) in select.group_by.iter().zip(&group_targets) {
        let index = match target {
            OrderByTarget::Column(idx) => *idx,
            OrderByTarget::Expr(e) => {
                let r = compile_value(em, reg, &table_scope, e)?;
                usize::try_from(r.saturating_sub(first)).unwrap_or(0)
            }
        };
        sort_keys.push(SortKeyColumn {
            index,
            descending: false,
            collation: collation_of(expr).unwrap_or(Collation::Binary),
            nulls_first: true,
        });
    }
    em.patch_p4(sorter_open_addr, P4::SortKey(sort_keys));

    let count = usize::try_from(reg.peek().saturating_sub(first)).unwrap_or(0);
    let record_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::MakeRecord,
        first,
        i32::try_from(count).unwrap_or(0),
        record_reg,
    ));
    em.emit(Instruction::new(
        Opcode::SorterInsert,
        cursors.sort,
        record_reg,
        0,
    ));

    em.place(scan_skip);
    let scan_next = em.emit(Instruction::new(Opcode::Next, cursors.table, 0, 0));
    em.patch_p2(scan_next, scan_loop);

    // Pass 2: walk the sorted buffer, grouping and aggregating.
    em.place(sort_step);
    let sort_addr = em.emit(Instruction::new(Opcode::SorterSort, cursors.sort, 0, 0));
    // `SorterSort` jumps straight past pass 2 when the sorter is empty
    // (a WHERE-matching-zero-rows table). An explicit `GROUP BY` in
    // that case has zero groups, so `end_label` is correct as-is; the
    // implicit whole-table group (#287) still needs its one all-NULL
    // (count(*) = 0) row, so it jumps to `tail_flush_label` below
    // instead — the same unconditional flush the normal end-of-loop
    // path falls through to.
    let empty_sorter_target = if implicit_group {
        em.new_label()
    } else {
        end_label
    };
    em.patch_p2(sort_addr, empty_sorter_target);

    let limit = compile_limit_setup(em, reg, &table_scope, select)?;

    let aggs = collect_aggregates(select)?;
    let zero_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Integer, 0, zero_reg, 0));
    let have_group_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Integer, 0, have_group_reg, 0));

    let prev_key_regs: Vec<i32> = group_targets.iter().map(|_| reg.alloc()).collect();
    let snapshot_regs: Vec<i32> = schema.columns.iter().map(|_| reg.alloc()).collect();
    // Initialized to NULL up front so the implicit whole-table group's
    // tail flush over a zero-row table (no row ever reaches
    // `read_row_columns_into` to overwrite these) reads NULL for any
    // plain (non-aggregate) column reference, matching SQLite's
    // "arbitrary row" semantics degrading to NULL when there is none.
    // Harmless for the explicit-`GROUP BY` case too: every real group
    // always has at least one row, which unconditionally overwrites
    // these before its flush.
    for &r in &snapshot_regs {
        em.emit(Instruction::new(Opcode::Null, 0, r, 0));
    }
    // Aggregate-context slots (`Vm::agg_contexts`) are a disjoint table
    // from the register file, addressed by their own small integer
    // space — a bare 0-based counter here, not `reg.alloc()`.
    let agg_slots: Vec<AggSlot> = aggs
        .into_iter()
        .enumerate()
        .map(|(slot, (call, name, arg))| {
            let slot = i32::try_from(slot).unwrap_or(0);
            AggSlot {
                call,
                name,
                arg,
                slot,
            }
        })
        .collect();

    let sorted_loop = em.new_label();
    em.place(sorted_loop);
    let sorter_data_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::SorterData,
        cursors.sort,
        sorter_data_reg,
        0,
    ));
    em.emit(Instruction::new(
        Opcode::OpenPseudo,
        cursors.pseudo,
        sorter_data_reg,
        0,
    ));

    // Compute this row's GROUP BY key into fresh registers.
    let cur_key_regs: Vec<i32> = group_targets
        .iter()
        .zip(&select.group_by)
        .map(|(target, expr)| match target {
            OrderByTarget::Column(idx) => {
                let r = reg.alloc();
                read_pseudo_column(em, schema, cursors.pseudo, *idx, r)?;
                Ok(r)
            }
            OrderByTarget::Expr(_) => compile_value(em, reg, &pseudo_scope, expr),
        })
        .collect::<Result<_, CodegenError>>()?;

    let group_key_p4s: Vec<P4> = select
        .group_by
        .iter()
        .map(|expr| {
            let collation = collation_of(expr).unwrap_or(Collation::Binary);
            let affinity = comparison_affinity(expr_affinity(&table_scope, expr), None);
            p4_coll_seq(collation, affinity)
        })
        .collect();

    let boundary_label = em.new_label();
    let not_boundary_label = em.new_label();
    let first_row_check = em.emit(Instruction::new(Opcode::Eq, have_group_reg, 0, zero_reg));
    em.patch_p2(first_row_check, boundary_label);
    for ((&cur, &prev), p4) in cur_key_regs.iter().zip(&prev_key_regs).zip(&group_key_p4s) {
        let a_null = em.new_label();
        let same_col = em.new_label();
        let a_null_addr = em.emit(Instruction::new(Opcode::IsNull, cur, 0, 0));
        em.patch_p2(a_null_addr, a_null);
        let b_null_addr = em.emit(Instruction::new(Opcode::IsNull, prev, 0, 0));
        em.patch_p2(b_null_addr, boundary_label);
        let eq_addr = em.emit(Instruction::with_p4(Opcode::Eq, cur, 0, prev, p4.clone()));
        em.patch_p2(eq_addr, same_col);
        let goto_boundary = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
        em.patch_p2(goto_boundary, boundary_label);
        em.place(a_null);
        let b_not_null_addr = em.emit(Instruction::new(Opcode::NotNull, prev, 0, 0));
        em.patch_p2(b_not_null_addr, boundary_label);
        em.place(same_col);
    }
    let goto_not_boundary = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
    em.patch_p2(goto_not_boundary, not_boundary_label);

    em.place(boundary_label);
    let skip_flush = em.new_label();
    let flush_check = em.emit(Instruction::new(Opcode::Eq, have_group_reg, 0, zero_reg));
    em.patch_p2(flush_check, skip_flush);
    flush_group(
        em,
        reg,
        select,
        schema,
        catalog,
        &snapshot_regs,
        &agg_slots,
        limit.as_ref(),
        end_label,
        sink,
    )?;
    em.place(skip_flush);
    for (&cur, &prev) in cur_key_regs.iter().zip(&prev_key_regs) {
        em.emit(Instruction::new(Opcode::Copy, cur, prev, 0));
    }
    em.emit(Instruction::new(Opcode::Integer, 1, have_group_reg, 0));
    // This is a *new* group's first row: fold it in with `reset: true`
    // so a slot number reused from the previous group starts a fresh
    // accumulator rather than continuing the old one — then skip the
    // plain (non-reset) fold below, which is only for a group's
    // second-and-later rows.
    for agg in &agg_slots {
        emit_agg_step(em, reg, &pseudo_scope, agg, true)?;
    }
    let after_accumulate = em.new_label();
    let goto_after_accumulate = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
    em.patch_p2(goto_after_accumulate, after_accumulate);

    em.place(not_boundary_label);
    for agg in &agg_slots {
        emit_agg_step(em, reg, &pseudo_scope, agg, false)?;
    }

    em.place(after_accumulate);
    read_row_columns_into(em, schema, cursors.pseudo, &snapshot_regs)?;

    let sorted_next = em.emit(Instruction::new(Opcode::SorterNext, cursors.sort, 0, 0));
    em.patch_p2(sorted_next, sorted_loop);

    // Tail flush: the very last group never sees another row to trigger
    // `boundary_label`'s mid-loop flush. An explicit `GROUP BY` over
    // zero matching rows correctly produces zero groups (skip the
    // flush when `have_group_reg` never went high); the implicit
    // whole-table group (#287) always flushes exactly one row, even
    // over an empty table — `count(*)` finalizes to 0, other
    // aggregates to NULL, via `snapshot_regs`'s NULL initialization
    // above and `AggFinal`'s never-stepped-slot handling.
    if implicit_group {
        em.place(empty_sorter_target);
    }
    let skip_tail_flush = em.new_label();
    if !implicit_group {
        let tail_check = em.emit(Instruction::new(Opcode::Eq, have_group_reg, 0, zero_reg));
        em.patch_p2(tail_check, skip_tail_flush);
    }
    flush_group(
        em,
        reg,
        select,
        schema,
        catalog,
        &snapshot_regs,
        &agg_slots,
        limit.as_ref(),
        end_label,
        sink,
    )?;
    em.place(skip_tail_flush);
    Ok(())
}

/// Compiles an explicit `GROUP BY <indexed col(s)>` (#310) as a direct
/// index b-tree walk feeding [`compile_grouped_scan`]'s pass-2
/// boundary-detection/accumulate/flush logic directly, in place of pass
/// 1's `SorterOpen`/full-table-buffer/`SorterSort` — mirroring #296's
/// [`super::index_scan::try_compile_index_ordered_scan`] MVP, but for
/// `GROUP BY` instead of `ORDER BY`. Since the index already produces
/// rows in group-key order, there is nothing to sort: each row is
/// fetched straight off `cursors.table` (via `IdxRowid` + `SeekRowid`,
/// same as the `ORDER BY` fast path) and read directly through
/// `table_scope`, with no pseudo cursor, no `MakeRecord`, and no
/// sorter at all — `cursors.table` plays the role `cursors.pseudo`
/// plays in [`compile_grouped_scan`]'s pass 2, since both are simply
/// "the cursor positioned on the current row" as far as
/// `read_row_columns_into`/[`emit_agg_step`] are concerned.
///
/// Returns `Ok(true)` when this fast path was taken; `Ok(false)` leaves
/// `em`/`reg` untouched so the caller falls back to
/// [`compile_grouped_scan`]. MVP guardrail (matching #296's own): only
/// taken with no `WHERE` clause (no cardinality estimation to judge an
/// index scan against a filtered table scan), an ordinary rowid table,
/// and every `GROUP BY` term a bare column (a computed `GROUP BY`
/// expression has no corresponding index column to match against).
#[allow(clippy::too_many_arguments)]
pub(super) fn try_compile_index_ordered_group_by<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    implicit_group: bool,
    sink: &mut F,
) -> Result<bool, CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    if implicit_group || select.group_by.is_empty() {
        // No GROUP BY key at all: nothing for an index to order by,
        // and #287's implicit whole-table group has exactly one group
        // regardless — sorting a single group is already free.
        return Ok(false);
    }
    if select.where_clause.is_some() || schema.without_rowid {
        return Ok(false);
    }
    let group_targets: Vec<OrderByTarget> = select
        .group_by
        .iter()
        .map(|expr| order_by_target_for_expr(expr, schema))
        .collect::<Result<_, _>>()?;
    // Every target must be a bare column — a computed GROUP BY
    // expression has no corresponding index column to match against.
    // Collected into `group_col_indices` up front (rather than
    // re-matching `OrderByTarget::Column` inside the per-row loop
    // below) so that loop has no "already-rejected, can't happen"
    // branch to justify with an `unreachable!` the qualified-subset
    // gate (`make mvl-limit`) doesn't allow.
    let Some(group_col_indices): Option<Vec<usize>> = group_targets
        .iter()
        .map(|t| match t {
            OrderByTarget::Column(idx) => Some(*idx),
            OrderByTarget::Expr(_) => None,
        })
        .collect()
    else {
        return Ok(false);
    };
    let plans: Vec<OrderByPlan> = select
        .group_by
        .iter()
        .zip(&group_targets)
        .map(|(expr, target)| OrderByPlan {
            target: target.clone(),
            descending: false,
            collation: collation_of(expr).unwrap_or(Collation::Binary),
            nulls_first: true,
        })
        .collect();
    let Some((index_idx, forward)) = super::index_scan::find_ordering_index(schema, &plans) else {
        return Ok(false);
    };
    let Some(index) = schema.indexes.get(index_idx) else {
        return Ok(false);
    };

    let table_scope = Scope::single(schema, cursors.table).with_catalog(catalog.to_vec());

    // No dedicated cursor slot exists for this path's index cursor —
    // reuse the sort cursor number, since `SorterOpen`/`SorterInsert`
    // never run on this branch (matching #296's own convention).
    let index_cursor = cursors.sort;
    let root_page = i32::try_from(index.root_page).unwrap_or(0);
    let mut open_instr = Instruction::new(Opcode::OpenRead, index_cursor, root_page, 0);
    open_instr.p5 = 1;
    em.emit(open_instr);

    let limit = compile_limit_setup(em, reg, &table_scope, select)?;

    let aggs = collect_aggregates(select)?;
    let zero_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Integer, 0, zero_reg, 0));
    let have_group_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Integer, 0, have_group_reg, 0));

    let prev_key_regs: Vec<i32> = group_targets.iter().map(|_| reg.alloc()).collect();
    let snapshot_regs: Vec<i32> = schema.columns.iter().map(|_| reg.alloc()).collect();
    for &r in &snapshot_regs {
        em.emit(Instruction::new(Opcode::Null, 0, r, 0));
    }
    let agg_slots: Vec<AggSlot> = aggs
        .into_iter()
        .enumerate()
        .map(|(slot, (call, name, arg))| {
            let slot = i32::try_from(slot).unwrap_or(0);
            AggSlot {
                call,
                name,
                arg,
                slot,
            }
        })
        .collect();

    let (rewind_op, next_op) = if forward {
        (Opcode::IdxRewind, Opcode::IdxNext)
    } else {
        (Opcode::IdxLast, Opcode::IdxPrev)
    };
    let empty_index_target = end_label;
    let rewind_addr = em.emit(Instruction::new(rewind_op, index_cursor, 0, 0));
    em.patch_p2(rewind_addr, empty_index_target);

    let indexed_loop = em.new_label();
    em.place(indexed_loop);
    let rowid_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::IdxRowid,
        index_cursor,
        rowid_reg,
        0,
    ));
    let row_skip = em.new_label();
    let table_seek_addr = em.emit(Instruction::new(
        Opcode::SeekRowid,
        cursors.table,
        0,
        rowid_reg,
    ));
    em.patch_p2(table_seek_addr, row_skip);

    // Compute this row's GROUP BY key straight off the table cursor —
    // `group_col_indices` (checked above) means this is always a plain
    // column read, never `compile_value`.
    let cur_key_regs: Vec<i32> = group_col_indices
        .iter()
        .map(|&idx| {
            let r = reg.alloc();
            read_pseudo_column(em, schema, cursors.table, idx, r)?;
            Ok(r)
        })
        .collect::<Result<_, CodegenError>>()?;

    let group_key_p4s: Vec<P4> = select
        .group_by
        .iter()
        .map(|expr| {
            let collation = collation_of(expr).unwrap_or(Collation::Binary);
            let affinity = comparison_affinity(expr_affinity(&table_scope, expr), None);
            p4_coll_seq(collation, affinity)
        })
        .collect();

    let boundary_label = em.new_label();
    let not_boundary_label = em.new_label();
    let first_row_check = em.emit(Instruction::new(Opcode::Eq, have_group_reg, 0, zero_reg));
    em.patch_p2(first_row_check, boundary_label);
    for ((&cur, &prev), p4) in cur_key_regs.iter().zip(&prev_key_regs).zip(&group_key_p4s) {
        let a_null = em.new_label();
        let same_col = em.new_label();
        let a_null_addr = em.emit(Instruction::new(Opcode::IsNull, cur, 0, 0));
        em.patch_p2(a_null_addr, a_null);
        let b_null_addr = em.emit(Instruction::new(Opcode::IsNull, prev, 0, 0));
        em.patch_p2(b_null_addr, boundary_label);
        let eq_addr = em.emit(Instruction::with_p4(Opcode::Eq, cur, 0, prev, p4.clone()));
        em.patch_p2(eq_addr, same_col);
        let goto_boundary = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
        em.patch_p2(goto_boundary, boundary_label);
        em.place(a_null);
        let b_not_null_addr = em.emit(Instruction::new(Opcode::NotNull, prev, 0, 0));
        em.patch_p2(b_not_null_addr, boundary_label);
        em.place(same_col);
    }
    let goto_not_boundary = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
    em.patch_p2(goto_not_boundary, not_boundary_label);

    em.place(boundary_label);
    let skip_flush = em.new_label();
    let flush_check = em.emit(Instruction::new(Opcode::Eq, have_group_reg, 0, zero_reg));
    em.patch_p2(flush_check, skip_flush);
    flush_group(
        em,
        reg,
        select,
        schema,
        catalog,
        &snapshot_regs,
        &agg_slots,
        limit.as_ref(),
        end_label,
        sink,
    )?;
    em.place(skip_flush);
    for (&cur, &prev) in cur_key_regs.iter().zip(&prev_key_regs) {
        em.emit(Instruction::new(Opcode::Copy, cur, prev, 0));
    }
    em.emit(Instruction::new(Opcode::Integer, 1, have_group_reg, 0));
    for agg in &agg_slots {
        emit_agg_step(em, reg, &table_scope, agg, true)?;
    }
    let after_accumulate = em.new_label();
    let goto_after_accumulate = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
    em.patch_p2(goto_after_accumulate, after_accumulate);

    em.place(not_boundary_label);
    for agg in &agg_slots {
        emit_agg_step(em, reg, &table_scope, agg, false)?;
    }

    em.place(after_accumulate);
    read_row_columns_into(em, schema, cursors.table, &snapshot_regs)?;

    em.place(row_skip);
    let idx_next_addr = em.emit(Instruction::new(next_op, index_cursor, 0, 0));
    em.patch_p2(idx_next_addr, indexed_loop);

    // Tail flush: the very last group never sees another row to
    // trigger `boundary_label`'s mid-loop flush. Zero matching rows
    // (an empty table) means `empty_index_target == end_label` was
    // taken above, so this tail flush is only reached having seen at
    // least one row — `have_group_reg` is unconditionally set by then,
    // matching the explicit-`GROUP BY` (non-implicit) case in
    // `compile_grouped_scan`.
    flush_group(
        em,
        reg,
        select,
        schema,
        catalog,
        &snapshot_regs,
        &agg_slots,
        limit.as_ref(),
        end_label,
        sink,
    )?;
    Ok(true)
}

/// One aggregate call's `AggStep`/`AggFinal` binding (#263): `name`
/// selects the accumulator kind in `crate::vdbe::aggregate`, `arg` is
/// its single argument expression (`None` only for `count(*)`), and
/// `slot` is this call's aggregate-context slot number — the `AggStep`/
/// `AggFinal` analogue of the old `AggSlot`'s `primary` register, but
/// addressing `Vm::agg_contexts` (a disjoint table from the register
/// file) instead.
pub(super) struct AggSlot {
    call: Expr,
    name: String,
    arg: Option<Expr>,
    slot: i32,
}

/// Recognizes `expr` as an aggregate call this compiler can accumulate,
/// or reports why not. Only called on expressions [`find_aggregates`]
/// already identified as `is_aggregate_call`, so the "not an aggregate
/// at all" case can't happen here. Only `count`/`sum`/`avg`/`min`/`max`
/// have a `crate::vdbe::aggregate::AggState` accumulator today — same
/// set the old register-arithmetic scheme supported.
pub(super) fn classify_aggregate(expr: &Expr) -> Result<(String, Option<Expr>), CodegenError> {
    let ExprKind::FunctionCall { name, args, .. } = &expr.kind else {
        return Err(CodegenError::Unsupported {
            reason: "classify_aggregate called on a non-call expression".to_string(),
        });
    };
    let arg = match args {
        FunctionArgs::Star => None,
        FunctionArgs::List(list) if list.len() <= 1 => list.first().cloned(),
        FunctionArgs::List(_) => {
            return Err(CodegenError::Unsupported {
                reason: format!(
                    "aggregate function {} with more than one argument is not yet supported",
                    name.to_ascii_lowercase()
                ),
            })
        }
    };
    let name = name.to_ascii_lowercase();
    match name.as_str() {
        "count" => {}
        "sum" | "avg" | "min" | "max" if arg.is_some() => {}
        "sum" | "avg" | "min" | "max" => {
            return Err(CodegenError::Unsupported {
                reason: format!("{name}() requires a single argument"),
            })
        }
        other => {
            return Err(CodegenError::Unsupported {
                reason: format!("aggregate function {other} not yet supported in GROUP BY"),
            })
        }
    }
    Ok((name, arg))
}

/// Finds every aggregate-call sub-expression reachable from `select`'s
/// result columns and `HAVING` clause through `Paren`/`Collate`/`Unary`/
/// `Binary` wrappers (see [`compile_grouped_scan`]'s doc comment for the
/// bound), deduplicated by AST equality so `HAVING count(*) > 1` sharing
/// a call with a `count(*)` result column accumulates into one slot.
pub(super) fn collect_aggregates(
    select: &Select,
) -> Result<Vec<(Expr, String, Option<Expr>)>, CodegenError> {
    let mut found: Vec<Expr> = Vec::new();
    for col in &select.columns {
        if let ResultColumn::Expr { expr, .. } = col {
            find_aggregates(expr, &mut found);
        }
    }
    if let Some(having) = &select.having {
        find_aggregates(having, &mut found);
    }
    found
        .into_iter()
        .map(|call| {
            let (name, arg) = classify_aggregate(&call)?;
            Ok((call, name, arg))
        })
        .collect()
}

/// Whether `select` has any aggregate call in its result columns or
/// `HAVING` clause — the #287 trigger for compiling an implicit
/// whole-table group when `select.group_by.is_empty()`, distinguishing
/// `SELECT count(*) FROM t;` (implicit group) from an ordinary
/// aggregate-free `SELECT` (plain scan).
pub(super) fn select_has_aggregate(select: &Select) -> bool {
    let mut found = Vec::new();
    for col in &select.columns {
        if let ResultColumn::Expr { expr, .. } = col {
            find_aggregates(expr, &mut found);
            if !found.is_empty() {
                return true;
            }
        }
    }
    if let Some(having) = &select.having {
        find_aggregates(having, &mut found);
    }
    !found.is_empty()
}

pub(super) fn find_aggregates(expr: &Expr, out: &mut Vec<Expr>) {
    if let ExprKind::FunctionCall { name, args, .. } = &expr.kind {
        if is_aggregate_call(name, args) {
            if !out.contains(expr) {
                out.push(expr.clone());
            }
            return;
        }
    }
    match &expr.kind {
        ExprKind::Paren(inner) | ExprKind::Collate { expr: inner, .. } => {
            find_aggregates(inner, out);
        }
        ExprKind::Unary { expr: inner, .. } => find_aggregates(inner, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            find_aggregates(lhs, out);
            find_aggregates(rhs, out);
        }
        _ => {}
    }
}

/// Rewrites every aggregate-call sub-expression matching one of
/// `agg_slots` into a `Column` reference to that slot's synthetic
/// output-record field (see [`flush_group`]), so the rewritten
/// expression can compile against the flush-time synthetic
/// schema/record via the ordinary (aggregate-unaware) `compile_value`/
/// `compile_cond` machinery.
pub(super) fn substitute_aggregates(
    expr: &Expr,
    agg_slots: &[AggSlot],
    synthetic_names: &[String],
) -> Expr {
    if let Some(pos) = agg_slots.iter().position(|slot| slot.call == *expr) {
        return Expr {
            kind: ExprKind::Column {
                table: None,
                catalog: None,
                name: synthetic_names.get(pos).cloned().unwrap_or_default(),
            },
            span: expr.span,
        };
    }
    let kind = match &expr.kind {
        ExprKind::Paren(inner) => ExprKind::Paren(Box::new(substitute_aggregates(
            inner,
            agg_slots,
            synthetic_names,
        ))),
        ExprKind::Collate {
            expr: inner,
            collation,
        } => ExprKind::Collate {
            expr: Box::new(substitute_aggregates(inner, agg_slots, synthetic_names)),
            collation: collation.clone(),
        },
        ExprKind::Unary { op, expr: inner } => ExprKind::Unary {
            op: *op,
            expr: Box::new(substitute_aggregates(inner, agg_slots, synthetic_names)),
        },
        ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
            op: *op,
            lhs: Box::new(substitute_aggregates(lhs, agg_slots, synthetic_names)),
            rhs: Box::new(substitute_aggregates(rhs, agg_slots, synthetic_names)),
        },
        other => other.clone(),
    };
    Expr {
        kind,
        span: expr.span,
    }
}

/// Pseudo-cursor-safe single-column read: like `emit_column_read`, but
/// aware that `cursor` re-reads an already-materialized record (so the
/// rowid-alias column is an ordinary field within it, not something
/// `Opcode::Rowid` can fetch) — see `compile_row_values`'s identical
/// special case for why.
pub(super) fn read_pseudo_column(
    em: &mut Emitter,
    schema: &TableSchema,
    cursor: i32,
    idx: usize,
    dest: i32,
) -> Result<(), CodegenError> {
    if rowid_alias_column(schema) == Some(idx) {
        em.emit(Instruction::new(
            Opcode::Column,
            cursor,
            i32::try_from(idx).map_err(|_| CodegenError::Unsupported {
                reason: format!("column index {idx} does not fit in a P2 operand"),
            })?,
            dest,
        ));
        return Ok(());
    }
    emit_column_read(em, schema, cursor, idx, dest)
}

/// Reads every one of `schema`'s columns from the pass-2 pseudo cursor
/// into the given (already-allocated, persistent) destination
/// registers — the per-row snapshot `compile_grouped_scan` keeps so a
/// plain (non-aggregate) result/`HAVING` column reads the group's last
/// row, matching SQLite's own "arbitrary row" semantics for a
/// non-grouped-by column.
pub(super) fn read_row_columns_into(
    em: &mut Emitter,
    schema: &TableSchema,
    cursor: i32,
    dest: &[i32],
) -> Result<(), CodegenError> {
    for (idx, &r) in dest.iter().enumerate() {
        read_pseudo_column(em, schema, cursor, idx, r)?;
    }
    Ok(())
}

/// Emits one `AggStep` for `agg`'s slot (#263): compiles `agg.arg` (if
/// any) into a fresh register and folds it via `Opcode::AggStep`,
/// exactly the shape `crate::vdbe::exec::agg_step` expects — a
/// contiguous argument-register run starting at `P2`, arity/name via
/// `P4::AggFunc`. `reset` sets `P5`, which discards this slot's prior
/// state before folding (`Vm`'s "start a fresh accumulator" behavior)
/// — the group-boundary row for a reused slot number passes `true`;
/// every other row in the same group passes `false`.
///
/// `min`/`max` compare under `agg.arg`'s collation (an explicit
/// `COLLATE` wrapper only, same resolution `collation_of` gives the
/// scalar comparison path — see #265). Unlike that scalar path, this
/// does not also apply a comparison *affinity* first:
/// `crate::vdbe::aggregate::step`'s `compare` call has no affinity
/// parameter to feed one to, a pre-existing gap in the `AggStep`/
/// `AggFinal` opcode contract (not introduced by this ticket, and not
/// regressed from the old register-arithmetic scheme, which also had
/// no affinity handling on its `Lt`/`Gt` compares before #265's
/// collation-only fix).
pub(super) fn emit_agg_step(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    scope: &Scope,
    agg: &AggSlot,
    reset: bool,
) -> Result<(), CodegenError> {
    let (arg_reg, arity, collation) = match &agg.arg {
        Some(expr) => {
            let collation = collation_of(expr).unwrap_or(Collation::Binary);
            (
                Some(compile_value(em, reg, scope, expr)?),
                1usize,
                collation,
            )
        }
        None => (None, 0usize, Collation::Binary),
    };
    let p2 = arg_reg.unwrap_or(0);
    let mut instr = Instruction::with_p4(
        Opcode::AggStep,
        agg.slot,
        p2,
        0,
        P4::AggFunc {
            name: agg.name.clone(),
            arity,
            collation,
        },
    );
    if reset {
        instr.p5 = 1;
    }
    em.emit(instr);
    Ok(())
}

/// Finalizes and emits one grouped output row via `sink`, applying
/// `HAVING`/`LIMIT`/`OFFSET` exactly as the ungrouped scans do. Builds a
/// synthetic record — the group's snapshot column values (from the last
/// row seen) followed by each aggregate's finalized value — and opens a
/// fresh pseudo cursor over it, so `select.columns`/`having` (with
/// aggregate calls rewritten to reference the synthetic record's
/// trailing fields via [`substitute_aggregates`]) compile through the
/// ordinary `compile_row_values`/`compile_cond` machinery unchanged.
#[allow(clippy::too_many_arguments)]
pub(super) fn flush_group<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    catalog: &[TableSchema],
    snapshot_regs: &[i32],
    agg_slots: &[AggSlot],
    limit: Option<&LimitState>,
    end_label: Label,
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    let synthetic_names: Vec<String> = (0..agg_slots.len()).map(|i| format!("__agg{i}")).collect();

    let mut synthetic_columns = schema.columns.clone();
    synthetic_columns.extend(synthetic_names.iter().cloned());
    let mut synthetic_types = schema.column_types.clone();
    synthetic_types.extend(synthetic_names.iter().map(|_| String::new()));
    let synthetic_schema = TableSchema {
        name: schema.name.clone(),
        root_page: 0,
        columns: synthetic_columns,
        without_rowid: schema.without_rowid,
        strict: false,
        column_types: synthetic_types,
        is_virtual: false,
        sql: String::new(),
        indexes: Vec::new(),
    };

    // Allocate one fresh, contiguous register per snapshot/aggregate
    // field up front — `reg.alloc()` bump-allocates sequentially, so as
    // long as nothing else allocates in between, `dests` is guaranteed
    // contiguous for `MakeRecord`.
    let synthetic_count = snapshot_regs.len().saturating_add(agg_slots.len());
    let dests: Vec<i32> = (0..synthetic_count).map(|_| reg.alloc()).collect();
    let synthetic_first = dests.first().copied().unwrap_or_else(|| reg.alloc());
    for (&snap, &dest) in snapshot_regs.iter().zip(&dests) {
        em.emit(Instruction::new(Opcode::Copy, snap, dest, 0));
    }
    let agg_dests = dests.get(snapshot_regs.len()..).unwrap_or(&[]);
    for (agg, &dest) in agg_slots.iter().zip(agg_dests) {
        // `avg()`'s sum/count division now happens inside
        // `crate::vdbe::aggregate::finalize` — `AggFinal` just reads
        // the slot's already-finalized value straight into `dest`.
        let arity = usize::from(agg.arg.is_some());
        em.emit(Instruction::with_p4(
            Opcode::AggFinal,
            agg.slot,
            0,
            dest,
            P4::Str(format!("{}({arity})", agg.name)),
        ));
    }
    let record_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::MakeRecord,
        synthetic_first,
        i32::try_from(synthetic_count).unwrap_or(0),
        record_reg,
    ));
    let flush_cursor = FLUSH_CURSOR;
    em.emit(Instruction::new(
        Opcode::OpenPseudo,
        flush_cursor,
        record_reg,
        0,
    ));

    let flush_scope = Scope::single(&synthetic_schema, flush_cursor).with_catalog(catalog.to_vec());
    let skip_label = em.new_label();
    if let Some(having) = &select.having {
        let rewritten = substitute_aggregates(having, agg_slots, &synthetic_names);
        compile_cond(
            em,
            reg,
            &flush_scope,
            &rewritten,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(skip_label)),
        )?;
    }
    if let Some(limit) = limit {
        emit_offset_guard(em, limit, skip_label);
    }
    if let Some(limit) = limit {
        emit_limit_guard(em, limit, end_label);
    }

    let rewritten_columns: Vec<ResultColumn> = select
        .columns
        .iter()
        .map(|col| match col {
            ResultColumn::Expr { expr, alias } => ResultColumn::Expr {
                expr: substitute_aggregates(expr, agg_slots, &synthetic_names),
                alias: alias.clone(),
            },
            other => other.clone(),
        })
        .collect();
    let throwaway = Select {
        distinct: None,
        columns: rewritten_columns,
        from: None,
        where_clause: None,
        group_by: Vec::new(),
        having: None,
        compound: Vec::new(),
        order_by: Vec::new(),
        limit: None,
        span: select.span,
    };
    let cols = result_columns(&throwaway, &synthetic_schema);
    let (proj_first, proj_count) = compile_row_values(
        em,
        reg,
        &synthetic_schema,
        &cols,
        flush_cursor,
        true,
        catalog,
    )?;
    sink(em, reg, proj_first, i32::try_from(proj_count).unwrap_or(0))?;
    em.place(skip_label);
    Ok(())
}

/// A cursor number for `flush_group`'s synthetic per-group record —
/// distinct from [`ScanCursors`]'s four numbers (0-3), which stay live
/// across every `flush_group` call within the same grouped scan.
pub(super) const FLUSH_CURSOR: i32 = 4;
