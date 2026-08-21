use super::limit_scan::{compile_limit_setup, emit_limit_guard, emit_offset_guard};
use super::order_by::{OrderByPlan, OrderByTarget};
use super::projection::emit_row_via_sink;
use super::*;

/// Finds a single index on `schema` whose declared column order is a
/// prefix match (case-insensitively, column-for-column) for `plans` — the
/// resolved `ORDER BY` term list — either forward (the index's own
/// ascending/descending declaration for each column already matches the
/// requested direction) or backward (every column's requested direction
/// is the exact reverse of the index's declaration). Returns the index
/// plus whether walking it needs to go forward (ascending b-tree order)
/// or backward (descending) to produce `plans`' requested order.
///
/// Deliberately narrow (#296 MVP): only a bare column target (no
/// computed `ORDER BY` expression), `BINARY` collation only (matching
/// `src/btree/index.rs`'s own BINARY-only key comparison — a `NOCASE`/
/// other-collation `ORDER BY` can't be satisfied by walking the index's
/// raw byte order), and a `NULLS FIRST`/`NULLS LAST` clause that agrees
/// with the direction's default (an explicit clause overriding that
/// default has no way to be expressed by a plain b-tree walk). A mixed
/// per-column direction across `plans` that doesn't uniformly match (or
/// uniformly reverse) the index's own per-column directions also falls
/// through to `None` — the sorter path handles it instead.
pub(super) fn index_matches_ordering(
    schema: &TableSchema,
    index: &IndexSchema,
    plans: &[OrderByPlan],
) -> Option<bool> {
    if index.columns.len() < plans.len() {
        return None;
    }
    let mut forward: Option<bool> = None;
    for (i, plan) in plans.iter().enumerate() {
        if plan.collation != Collation::Binary {
            return None;
        }
        if plan.nulls_first == plan.descending {
            // An explicit NULLS clause overriding the direction's
            // default can't be expressed by a raw b-tree walk.
            return None;
        }
        let OrderByTarget::Column(col_idx) = &plan.target else {
            return None;
        };
        let col_name = schema.columns.get(*col_idx)?;
        let index_col = index.columns.get(i)?;
        if !index_col.name.eq_ignore_ascii_case(col_name) {
            return None;
        }
        let this_forward = plan.descending == index_col.desc;
        match forward {
            None => forward = Some(this_forward),
            Some(f) if f == this_forward => {}
            _ => return None,
        }
    }
    forward
}

pub(super) fn find_ordering_index(
    schema: &TableSchema,
    plans: &[OrderByPlan],
) -> Option<(usize, bool)> {
    if plans.is_empty() {
        return None;
    }
    schema.indexes.iter().enumerate().find_map(|(i, index)| {
        index_matches_ordering(schema, index, plans).map(|forward| (i, forward))
    })
}

/// Compiles `SELECT ... ORDER BY <indexed col(s)> [DESC] LIMIT n [OFFSET
/// m]` (#296) as a direct index b-tree walk — `IdxRewind`/`IdxNext`
/// (forward) or `IdxLast`/`IdxPrev` (backward), `IdxRowid` +
/// `SeekRowid` to fetch the full row, LIMIT/OFFSET as an early-exit
/// guard during the walk — in place of `compile_sorted_scan`'s
/// `Rewind`/`Next` + sorter pipeline. No buffering, no sort, ever.
///
/// Returns `Ok(true)` when this fast path was taken; `Ok(false)` leaves
/// `em`/`reg` untouched so the caller falls back to
/// [`super::limit_scan::compile_sorted_scan`]. MVP guardrail (matching
/// the issue's bounded scope): only taken when there's no `WHERE` clause
/// this index couldn't also serve — since this MVP does no cardinality
/// estimation, "no WHERE at all" is the conservative stand-in for that
/// — and no `DISTINCT`/`WITHOUT ROWID` table (this path leans on
/// `SeekRowid`, which needs an ordinary rowid table).
#[allow(clippy::too_many_arguments)]
pub(super) fn try_compile_index_ordered_scan<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    order_by_plans: &[OrderByPlan],
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    sink: &mut F,
) -> Result<bool, CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    if select.where_clause.is_some() {
        return Ok(false);
    }
    if matches!(select.distinct, Some(Distinctness::Distinct)) {
        return Ok(false);
    }
    if schema.without_rowid {
        return Ok(false);
    }
    let Some((index_idx, forward)) = find_ordering_index(schema, order_by_plans) else {
        return Ok(false);
    };
    let Some(index) = schema.indexes.get(index_idx) else {
        return Ok(false);
    };

    // No dedicated cursor slot exists for this path's index cursor —
    // reuse the sort cursor number, since `compile_sorted_scan`'s
    // `SorterOpen`/`SorterInsert` never run on this branch.
    let index_cursor = cursors.sort;
    let root_page = i32::try_from(index.root_page).unwrap_or(0);
    let mut open_instr = Instruction::new(Opcode::OpenRead, index_cursor, root_page, 0);
    open_instr.p5 = 1;
    em.emit(open_instr);

    let scope = Scope::single(schema, cursors.table).with_catalog(catalog.to_vec());
    let limit = compile_limit_setup(em, reg, &scope, select)?;

    let (rewind_op, next_op) = if forward {
        (Opcode::IdxRewind, Opcode::IdxNext)
    } else {
        (Opcode::IdxLast, Opcode::IdxPrev)
    };
    let rewind_addr = em.emit(Instruction::new(rewind_op, index_cursor, 0, 0));
    em.patch_p2(rewind_addr, end_label);
    let loop_start = em.new_label();
    em.place(loop_start);

    let row_skip = em.new_label();
    let rowid_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::IdxRowid,
        index_cursor,
        rowid_reg,
        0,
    ));
    let table_seek_addr = em.emit(Instruction::new(
        Opcode::SeekRowid,
        cursors.table,
        0,
        rowid_reg,
    ));
    em.patch_p2(table_seek_addr, row_skip);

    if let Some(limit) = &limit {
        emit_offset_guard(em, limit, row_skip);
    }
    if let Some(limit) = &limit {
        emit_limit_guard(em, limit, end_label);
    }
    emit_row_via_sink(em, reg, select, schema, cursors.table, false, catalog, sink)?;

    em.place(row_skip);
    let next_addr = em.emit(Instruction::new(next_op, index_cursor, 0, 0));
    em.patch_p2(next_addr, loop_start);
    Ok(true)
}
