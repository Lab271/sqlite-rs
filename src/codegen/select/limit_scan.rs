use super::order_by::{OrderByPlan, OrderByTarget};
use super::projection::{
    compile_row_values, emit_distinct_guard, emit_row_via_sink, ResultColumnPlan,
};
use super::*;
use crate::planner::{is_skip_scan_worthwhile, Stats};
/// LIMIT/OFFSET counters, set up once before the scan loop starts.
pub(super) struct LimitState {
    offset_reg: Option<i32>,
    limit_reg: Option<i32>,
}

pub(super) fn compile_limit_setup(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    scope: &Scope,
    select: &Select,
) -> Result<Option<LimitState>, CodegenError> {
    let Some(limit) = &select.limit else {
        return Ok(None);
    };
    let limit_reg = compile_value(em, reg, scope, &limit.limit)?;
    let offset_reg = match &limit.offset {
        Some(offset_expr) => Some(compile_value(em, reg, scope, offset_expr)?),
        None => None,
    };
    Ok(Some(LimitState {
        offset_reg,
        limit_reg: Some(limit_reg),
    }))
}

/// Emits the OFFSET skip-guard (jumping to `row_skip` while
/// `offset_reg` still has rows to skip) — call once per scanned row,
/// before deciding whether to emit it.
pub(super) fn emit_offset_guard(em: &mut Emitter, limit: &LimitState, row_skip: Label) {
    if let Some(offset_reg) = limit.offset_reg {
        let addr = em.emit(Instruction::new(Opcode::IfPos, offset_reg, 0, 1));
        em.patch_p2(addr, row_skip);
    }
}

/// Emits the LIMIT stop-guard: call once per row *before* emitting it
/// (mirroring `emit_offset_guard`'s check-before-act shape, not
/// `emit_row_via_sink`'s old post-emit position). `IfNotZero` decrements
/// `limit_reg` only while it's positive and jumps whenever it's
/// nonzero — a negative `LIMIT` (SQLite's "no limit" convention) never
/// reaches zero and always falls into that jump, staying unbounded —
/// then a `Goto` reached only when `limit_reg` has hit exactly zero
/// stops the scan before this row is ever emitted.
///
/// This ordering matters for `LIMIT 0`: the old post-emit
/// `DecrJumpZero` never got a chance to run before the *first* row was
/// already emitted, so `LIMIT 0` emitted every row instead of none
/// (#129's benchmarking incidentally caught this pre-existing bug).
pub(super) fn emit_limit_guard(em: &mut Emitter, limit: &LimitState, end_label: Label) {
    if let Some(limit_reg) = limit.limit_reg {
        let has_budget_addr = em.emit(Instruction::new(Opcode::IfNotZero, limit_reg, 0, 0));
        let stop_addr = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
        em.patch_p2(stop_addr, end_label);
        let continue_label = em.new_label();
        em.patch_p2(has_budget_addr, continue_label);
        em.place(continue_label);
    }
}

/// The two sides of a top-level `=` expression, or `None` for any other
/// shape. Used by [`try_compile_rowid_seek`] to recognize `WHERE rowid =
/// <int literal>` / `WHERE rowid = ?` (#137).
///
/// Single input reference, so lifetime elision ties both tuple elements
/// to it without an explicit `<'a>` annotation — the qualified subset
/// (`make mvl-limit`) forbids explicit lifetimes, and a helper taking
/// both `schema` and `expr` by reference while returning a borrow of
/// `expr` alone would need one. The caller also needs `schema` (to pick
/// the non-rowid side via [`is_rowid_reference`]), so that step happens
/// in [`try_compile_rowid_seek`] itself, which already holds both.
pub(crate) fn top_level_equality_operands(expr: &Expr) -> Option<(&Expr, &Expr)> {
    let ExprKind::Binary {
        op: BinaryOp::Eq,
        lhs,
        rhs,
    } = &expr.kind
    else {
        return None;
    };
    Some((lhs, rhs))
}

pub(crate) fn is_rowid_reference(schema: &TableSchema, expr: &Expr) -> bool {
    let ExprKind::Column { name, .. } = &expr.kind else {
        return false;
    };
    if name.eq_ignore_ascii_case("rowid")
        || name.eq_ignore_ascii_case("_rowid_")
        || name.eq_ignore_ascii_case("oid")
    {
        return true;
    }
    rowid_alias_column(schema)
        .and_then(|idx| schema.columns.get(idx))
        .is_some_and(|col| col.eq_ignore_ascii_case(name))
}

/// Emits `Integer`/`Variable` + `SeekRowid` in place of the
/// `Rewind`/`Next` scan loop when `select`'s `WHERE` clause is a single
/// top-level equality between a rowid reference (the `rowid`/`_rowid_`/
/// `oid` keywords, or the table's actual `INTEGER PRIMARY KEY` alias
/// column) and an integer literal or bind parameter — O(log n) point
/// lookup instead of O(n) full scan (#137). Returns `Ok(true)` when the
/// fast path was taken; `Ok(false)` leaves `em`/`reg` untouched so the
/// caller falls back to the ordinary scan. Deliberately narrow —
/// secondary-index columns, ranges, and compound conditions (`AND`/`OR`)
/// all fall through to the ordinary scan and stay in V4 per the issue's
/// bounded scope.
#[allow(clippy::too_many_arguments)]
pub(super) fn try_compile_rowid_seek<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    sink: &mut F,
) -> Result<bool, CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    if matches!(select.distinct, Some(Distinctness::Distinct)) {
        // A single-row result is already distinct — but keeping this
        // path free of the ephemeral-index bookkeeping means it can
        // stay a straight-line seek. Not worth special-casing; DISTINCT
        // falls back to the ordinary scan.
        return Ok(false);
    }
    let Some(where_expr) = &select.where_clause else {
        return Ok(false);
    };
    let Some((lhs, rhs)) = top_level_equality_operands(where_expr) else {
        return Ok(false);
    };
    let operand = if is_rowid_reference(schema, lhs) {
        rhs
    } else if is_rowid_reference(schema, rhs) {
        lhs
    } else {
        return Ok(false);
    };
    // Bounded to the issue's in-scope shapes: an integer literal, or a
    // bare/numbered bind parameter. Anything else (a string literal
    // needing numeric-affinity coercion, a sub-expression, a named
    // parameter) falls back to the ordinary scan rather than risk
    // miscompiling a case this fast path wasn't built to handle.
    let is_supported_operand = matches!(
        &operand.kind,
        ExprKind::Literal(Literal::Integer(_))
            | ExprKind::Param(ParamKind::Anonymous | ParamKind::Numbered(_))
    );
    if !is_supported_operand {
        return Ok(false);
    }

    let scope = Scope::single(schema, cursors.table).with_catalog(catalog.to_vec());
    let limit = compile_limit_setup(em, reg, &scope, select)?;
    let value_reg = compile_value(em, reg, &scope, operand)?;
    let seek_addr = em.emit(Instruction::new(
        Opcode::SeekRowid,
        cursors.table,
        0,
        value_reg,
    ));
    em.patch_p2(seek_addr, end_label);

    let row_skip = em.new_label();
    if let Some(limit) = &limit {
        emit_offset_guard(em, limit, row_skip);
    }
    if let Some(limit) = &limit {
        emit_limit_guard(em, limit, end_label);
    }
    emit_row_via_sink(em, reg, select, schema, cursors.table, false, catalog, sink)?;
    em.place(row_skip);
    Ok(true)
}

/// Resolves every `select`'s result column to a bare, unqualified
/// column *name* — `None` if any result column is `*`/`table.*` or a
/// non-bare-column expression. Used by [`try_compile_covering_index_scan`]
/// to check "does this SELECT only ever need columns an index already
/// carries" without pulling in the general projection machinery, which
/// doesn't (yet) know how to read a computed expression off an index
/// record.
fn bare_result_column_names(select: &Select) -> Option<Vec<&str>> {
    select
        .columns
        .iter()
        .map(|col| match col {
            ResultColumn::Expr {
                expr:
                    Expr {
                        kind: ExprKind::Column { name, .. },
                        ..
                    },
                ..
            } => Some(name.as_str()),
            _ => None,
        })
        .collect()
}

fn where_col(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Column { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

/// A [`find_covering_index`] match: which of `schema.indexes` was
/// matched (by position, not reference — see that function's doc for
/// why) plus a clone of the probe operand expression.
pub(super) struct CoveringIndexMatch {
    pub(super) index_position: usize,
    pub(super) operand: Expr,
}

/// Finds an index of `schema` usable as a covering-index scan (#444,
/// non-`UNIQUE` indexes per #450) for `select`: `select.where_clause`
/// must be a single
/// top-level equality between the index's leading column and a
/// literal/bind-parameter operand, and every `SELECT`-list column (bare
/// columns only — see [`bare_result_column_names`]) must itself be
/// carried by that index. Returns the matched index's position in
/// `schema.indexes` plus the probe operand expression (owned, not
/// borrowed: a function taking two independent `&` parameters can't
/// return a value borrowing from either one without a named lifetime
/// tying them together, which the qualified subset forbids — see
/// `tools/mvl-limit`). Shared by [`try_compile_covering_index_scan`]
/// (which actually emits the scan) and `eqp.rs` (which only reports it),
/// so the two can never drift apart.
pub(super) fn find_covering_index(
    schema: &TableSchema,
    select: &Select,
) -> Option<CoveringIndexMatch> {
    if matches!(select.distinct, Some(Distinctness::Distinct)) {
        return None;
    }
    let where_expr = select.where_clause.as_ref()?;
    let (lhs, rhs) = top_level_equality_operands(where_expr)?;
    let (where_col_name, operand) = match (where_col(lhs), where_col(rhs)) {
        (Some(name), _) => (name, rhs),
        (_, Some(name)) => (name, lhs),
        _ => return None,
    };
    let is_supported_operand = matches!(
        &operand.kind,
        ExprKind::Literal(Literal::Integer(_))
            | ExprKind::Param(ParamKind::Anonymous | ParamKind::Numbered(_))
    );
    if !is_supported_operand {
        return None;
    }
    // A non-`UNIQUE` index's leading column can match more than one row
    // (#450) — `try_compile_covering_index_scan` walks every duplicate
    // via `IdxNext` after the initial `SeekIndexEq`, so uniqueness is no
    // longer a precondition here, just the leading-column match.
    let index_position = schema.indexes.iter().position(|idx| {
        idx.columns
            .first()
            .is_some_and(|c| c.name.eq_ignore_ascii_case(where_col_name))
    })?;
    let index = schema.indexes.get(index_position)?;
    let result_names = bare_result_column_names(select)?;
    let covers = |name: &str| {
        index
            .columns
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(name))
    };
    if !result_names.iter().all(|n| covers(n)) {
        return None;
    }
    Some(CoveringIndexMatch {
        index_position,
        operand: operand.clone(),
    })
}

/// Emits an index-only ("covering index") scan (#444) in place of
/// `SeekRowid` + full row decode, when [`find_covering_index`] finds an
/// index that carries every column this `SELECT` needs — `SeekIndexEq`
/// (the point probe) + `Column` reads straight out of the matched index
/// entry, never touching the table cursor at all. When the index isn't
/// `UNIQUE` (#450), the initial match may have duplicate-key siblings:
/// an `IdxNext` + leading-column-still-equal recheck loop walks and
/// emits each one, falling out (to `end_label`) the first time the
/// leading column no longer matches the probe (or the index is
/// exhausted) — a `UNIQUE` index's single match still falls out the same
/// way on its very first `IdxNext`, so this subsumes #444's original
/// single-probe behavior rather than branching around it.
///
/// Returns `Ok(true)` when this fast path was taken; `Ok(false)` leaves
/// `em`/`reg` untouched so the caller falls back to the ordinary scan
/// (or [`try_compile_rowid_seek`]).
#[allow(clippy::too_many_arguments)]
pub(super) fn try_compile_covering_index_scan<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    sink: &mut F,
) -> Result<bool, CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    let Some(CoveringIndexMatch {
        index_position,
        operand,
    }) = find_covering_index(schema, select)
    else {
        return Ok(false);
    };
    let Some(index) = schema.indexes.get(index_position) else {
        return Ok(false);
    };
    let operand = &operand;
    // Re-derived rather than threaded through `find_covering_index`'s
    // return value: `bare_result_column_names` is infallible once
    // `find_covering_index` has already returned `Some`.
    let result_names = bare_result_column_names(select).unwrap_or_default();

    let index_cursor = cursors.sort;
    let root_page = crate::codegen::index_maintenance::valid_index_root_page(index)?;
    let mut open_instr = Instruction::new(Opcode::OpenRead, index_cursor, root_page, 0);
    open_instr.p5 = 1;
    em.emit(open_instr);

    let scope = Scope::single(schema, cursors.table).with_catalog(catalog.to_vec());
    let limit = compile_limit_setup(em, reg, &scope, select)?;
    let value_reg = compile_value(em, reg, &scope, operand)?;
    let seek_addr = em.emit(Instruction::with_p4(
        Opcode::SeekIndexEq,
        index_cursor,
        0,
        value_reg,
        P4::Int(1),
    ));
    em.patch_p2(seek_addr, end_label);

    let loop_start = em.new_label();
    em.place(loop_start);

    let row_skip = em.new_label();
    if let Some(limit) = &limit {
        emit_offset_guard(em, limit, row_skip);
    }
    if let Some(limit) = &limit {
        emit_limit_guard(em, limit, end_label);
    }

    let mut regs = Vec::with_capacity(result_names.len());
    for name in &result_names {
        let col_idx = index
            .columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
            .unwrap_or(0);
        let r = reg.alloc();
        em.emit(Instruction::new(
            Opcode::Column,
            index_cursor,
            i32::try_from(col_idx).unwrap_or(0),
            r,
        ));
        regs.push(r);
    }
    let (first, count) = match regs.first() {
        Some(&first) => (first, i32::try_from(regs.len()).unwrap_or(0)),
        None => (reg.alloc(), 0),
    };
    sink(em, reg, first, count)?;

    // A `UNIQUE` index's single match falls straight out here on the
    // very first `IdxNext` (nothing shares its key); a non-`UNIQUE`
    // index's duplicate-key siblings loop back to `loop_start` for as
    // long as the leading column keeps matching the probe (#450).
    em.place(row_skip);
    let next_addr = em.emit(Instruction::new(Opcode::IdxNext, index_cursor, 0, 0));
    let recheck = em.new_label();
    em.patch_p2(next_addr, recheck);
    let exhausted = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
    em.patch_p2(exhausted, end_label);

    em.place(recheck);
    let leading = reg.alloc();
    em.emit(Instruction::new(Opcode::Column, index_cursor, 0, leading));
    let eq_addr = em.emit(Instruction::new(Opcode::Eq, leading, 0, value_reg));
    em.patch_p2(eq_addr, loop_start);

    Ok(true)
}

/// A [`find_skip_scan_index`] match: which of `schema.indexes` was
/// matched, the matched column's position *within that index*
/// (guaranteed `> 0` — position `0` is the leading column, already
/// handled by [`find_covering_index`]/[`try_compile_rowid_seek`]), and
/// a clone of the probe operand expression.
pub(super) struct SkipScanMatch {
    pub(super) index_position: usize,
    pub(super) column_position: usize,
    pub(super) operand: Expr,
}

/// Finds an index usable for a skip-scan (#485) over `select`:
/// `select.where_clause` must be a single top-level equality between a
/// *non-leading* column of one of `schema.indexes` and a literal/bind-
/// parameter operand, and [`is_skip_scan_worthwhile`] must judge the
/// index's leading column low-cardinality enough (per `stats`) for
/// walking the whole index to beat a full table scan. Deliberately
/// narrow, mirroring [`find_covering_index`]'s scope: an integer
/// literal or bind-parameter operand only, no `DISTINCT`, no `WITHOUT
/// ROWID` table (this path leans on `SeekRowid` to fetch the full row
/// once the index entry matches, same as
/// [`super::index_scan::try_compile_index_ordered_scan`]).
pub(super) fn find_skip_scan_index(
    schema: &TableSchema,
    select: &Select,
    stats: &Stats,
) -> Option<SkipScanMatch> {
    if matches!(select.distinct, Some(Distinctness::Distinct)) {
        return None;
    }
    if schema.without_rowid {
        return None;
    }
    let where_expr = select.where_clause.as_ref()?;
    let (lhs, rhs) = top_level_equality_operands(where_expr)?;
    let (where_col_name, operand) = match (where_col(lhs), where_col(rhs)) {
        (Some(name), _) => (name, rhs),
        (_, Some(name)) => (name, lhs),
        _ => return None,
    };
    let is_supported_operand = matches!(
        &operand.kind,
        ExprKind::Literal(Literal::Integer(_))
            | ExprKind::Param(ParamKind::Anonymous | ParamKind::Numbered(_))
    );
    if !is_supported_operand {
        return None;
    }
    schema
        .indexes
        .iter()
        .enumerate()
        .find_map(|(index_position, index)| {
            let column_position = index
                .columns
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(where_col_name))?;
            if column_position == 0 {
                // The leading column is a covering-index/rowid-seek match,
                // not a skip-scan one.
                return None;
            }
            if !is_skip_scan_worthwhile(&index.name, stats) {
                return None;
            }
            Some(SkipScanMatch {
                index_position,
                column_position,
                operand: operand.clone(),
            })
        })
}

/// Compiles a skip-scan (#485): a query filtering on a non-leading
/// column of a composite index, where [`find_skip_scan_index`] judges
/// the leading column's cardinality low enough that walking the whole
/// index (`IdxRewind`/`IdxNext`) — checking the matched column on each
/// narrower index entry, then `IdxRowid` + `SeekRowid` to fetch the
/// full row only for a match — beats a full `Rewind`/`Next` table scan
/// that decodes every column of every row. Unlike real SQLite's
/// skip-scan (a genuine per-distinct-leading-value binary seek),
/// `IndexCursor::seek` in this codebase is a documented Tier 0 linear
/// scan (`src/btree/index.rs`), so this walks every index entry rather
/// than truly skipping past a large group once it stops matching — the
/// win here comes from decoding narrower index rows and only touching
/// the table for genuine matches, not from sub-linear seeking.
///
/// Returns `Ok(true)` when this fast path was taken; `Ok(false)` leaves
/// `em`/`reg` untouched so the caller falls back to the ordinary scan.
#[allow(clippy::too_many_arguments)]
pub(super) fn try_compile_skip_scan_index<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    stats: &Stats,
    sink: &mut F,
) -> Result<bool, CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    let Some(SkipScanMatch {
        index_position,
        column_position,
        operand,
    }) = find_skip_scan_index(schema, select, stats)
    else {
        return Ok(false);
    };
    let Some(index) = schema.indexes.get(index_position) else {
        return Ok(false);
    };

    // No dedicated cursor slot exists for this path's index cursor —
    // reuse the sort cursor number, mirroring
    // `try_compile_index_ordered_scan`/`try_compile_covering_index_scan`
    // (neither of `SorterOpen`/`SorterInsert` ever runs on this branch).
    let index_cursor = cursors.sort;
    let root_page = crate::codegen::index_maintenance::valid_index_root_page(index)?;
    let mut open_instr = Instruction::new(Opcode::OpenRead, index_cursor, root_page, 0);
    open_instr.p5 = 1;
    em.emit(open_instr);

    let scope = Scope::single(schema, cursors.table).with_catalog(catalog.to_vec());
    let limit = compile_limit_setup(em, reg, &scope, select)?;
    let probe_reg = compile_value(em, reg, &scope, &operand)?;

    let rewind_addr = em.emit(Instruction::new(Opcode::IdxRewind, index_cursor, 0, 0));
    em.patch_p2(rewind_addr, end_label);
    let loop_start = em.new_label();
    em.place(loop_start);

    let row_skip = em.new_label();
    let col_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::Column,
        index_cursor,
        i32::try_from(column_position).unwrap_or(0),
        col_reg,
    ));
    let eq_addr = em.emit(Instruction::new(Opcode::Eq, col_reg, 0, probe_reg));
    let matched = em.new_label();
    em.patch_p2(eq_addr, matched);
    let mismatch_addr = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
    em.patch_p2(mismatch_addr, row_skip);

    em.place(matched);
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
    let next_addr = em.emit(Instruction::new(Opcode::IdxNext, index_cursor, 0, 0));
    em.patch_p2(next_addr, loop_start);
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compile_direct_scan<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    stats: &Stats,
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    if try_compile_rowid_seek(em, reg, select, schema, cursors, end_label, catalog, sink)? {
        return Ok(());
    }
    if try_compile_covering_index_scan(em, reg, select, schema, cursors, end_label, catalog, sink)?
    {
        return Ok(());
    }
    if try_compile_skip_scan_index(
        em, reg, select, schema, cursors, end_label, catalog, stats, sink,
    )? {
        return Ok(());
    }
    if matches!(select.distinct, Some(Distinctness::Distinct)) {
        em.emit(Instruction::new(
            Opcode::OpenEphemeral,
            cursors.distinct,
            0,
            0,
        ));
    }
    let scope = Scope::single(schema, cursors.table).with_catalog(catalog.to_vec());
    // #306: hoist any uncorrelated WHERE-clause IN/scalar subquery out
    // of the scan loop below, materializing it exactly once here rather
    // than on every outer row.
    let hoisted = match &select.where_clause {
        Some(where_expr) => crate::codegen::subquery::hoist_uncorrelated_where_subqueries(
            em, reg, &scope, where_expr,
        )?,
        None => std::collections::HashMap::new(),
    };
    let scope = scope.with_hoisted(std::rc::Rc::new(hoisted));
    // #314: memoize any correlated, single-outer-column WHERE-clause
    // scalar subquery against the scan's per-row correlated value,
    // instead of re-running it on every outer row.
    let memoized = match &select.where_clause {
        Some(where_expr) => crate::codegen::subquery::memoize_correlated_where_subqueries(
            em, reg, &scope, schema, where_expr,
        ),
        None => std::collections::HashMap::new(),
    };
    let scope = scope.with_memoized(std::rc::Rc::new(memoized));
    let limit = compile_limit_setup(em, reg, &scope, select)?;

    let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, cursors.table, 0, 0));
    em.patch_p2(rewind_addr, end_label);
    let loop_start = em.new_label();
    em.place(loop_start);

    let row_skip = em.new_label();
    if let Some(where_expr) = &select.where_clause {
        compile_cond(
            em,
            reg,
            &scope,
            where_expr,
            // `WHERE` is the boundary where SQL's three-valued logic
            // collapses to two: a predicate whose truth is unknown
            // excludes the row exactly like a false one.
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(row_skip)),
        )?;
    }
    emit_distinct_guard(
        em,
        reg,
        select,
        schema,
        cursors.table,
        false,
        cursors.distinct,
        row_skip,
        catalog,
    )?;
    if let Some(limit) = &limit {
        emit_offset_guard(em, limit, row_skip);
    }
    if let Some(limit) = &limit {
        emit_limit_guard(em, limit, end_label);
    }
    emit_row_via_sink(em, reg, select, schema, cursors.table, false, catalog, sink)?;

    em.place(row_skip);
    let next_addr = em.emit(Instruction::new(Opcode::Next, cursors.table, 0, 0));
    em.patch_p2(next_addr, loop_start);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compile_sorted_scan<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    order_by_plans: &[OrderByPlan],
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    if matches!(select.distinct, Some(Distinctness::Distinct)) {
        em.emit(Instruction::new(
            Opcode::OpenEphemeral,
            cursors.distinct,
            0,
            0,
        ));
    }

    let scope = Scope::single(schema, cursors.table).with_catalog(catalog.to_vec());
    // #306: same hoist as `compile_direct_scan` — see its comment.
    let hoisted = match &select.where_clause {
        Some(where_expr) => crate::codegen::subquery::hoist_uncorrelated_where_subqueries(
            em, reg, &scope, where_expr,
        )?,
        None => std::collections::HashMap::new(),
    };
    let scope = scope.with_hoisted(std::rc::Rc::new(hoisted));
    // #314: same memoization as `compile_direct_scan` — see its comment.
    let memoized = match &select.where_clause {
        Some(where_expr) => crate::codegen::subquery::memoize_correlated_where_subqueries(
            em, reg, &scope, schema, where_expr,
        ),
        None => std::collections::HashMap::new(),
    };
    let scope = scope.with_memoized(std::rc::Rc::new(memoized));

    // LIMIT/OFFSET are set up here, before the sorter opens, rather than
    // just before pass 2 — so a combined bound register is ready in time
    // to cap the sorter's buffer to the top-K rows it actually needs
    // (#129: an unbounded sort was the dominant cost of `ORDER BY ...
    // LIMIT N` on large tables). Reuses `OffsetLimit` (already
    // implemented for #89, previously unused by codegen — see this
    // module's doc comment) for its `-1`-means-unbounded convention
    // (`LIMIT -1`/no `LIMIT`), so the sorter falls back to its old
    // unbounded behavior automatically whenever the bound can't be
    // known to be safe. Bounding is skipped for `DISTINCT`: it dedupes
    // *after* the sort, so a bound applied before dedup could evict a
    // row DISTINCT would have deduped away, undercounting the result.
    let limit = compile_limit_setup(em, reg, &scope, select)?;
    let bound_reg = if matches!(select.distinct, Some(Distinctness::Distinct)) {
        None
    } else {
        limit.as_ref().map(|limit_state| {
            let offset_reg = limit_state.offset_reg.unwrap_or_else(|| {
                let zero = reg.alloc();
                em.emit(Instruction::new(Opcode::Integer, 0, zero, 0));
                zero
            });
            let combined = reg.alloc();
            em.emit(Instruction::new(
                Opcode::OffsetLimit,
                limit_state.limit_reg.unwrap_or(0),
                combined,
                offset_reg,
            ));
            combined
        })
    };

    // The sort-key descriptor (which register each term reads) isn't
    // known until pass 1 below actually allocates the computed-expression
    // registers, so `SorterOpen` is emitted with a placeholder P4 and
    // patched once that layout is known — it must still precede the scan
    // loop in program order.
    let mut sorter_open = Instruction::with_p4(Opcode::SorterOpen, cursors.sort, 0, 0, P4::None);
    if let Some(bound_reg) = bound_reg {
        sorter_open.p2 = bound_reg;
        sorter_open.p5 = 1;
    }
    let sorter_open_addr = em.emit(sorter_open);

    // Pass 1: buffer every matching row's full column tuple — plus a
    // trailing register per computed ORDER BY expression — into the
    // sorter, WHERE-filtered but pre-DISTINCT/LIMIT (those apply on
    // the sorted output, matching SQLite's own ORDER BY pipeline
    // shape). The trailing expression registers are never read back by
    // `sink` (it only ever projects `select.columns`), so they exist
    // purely as sort keys.
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
            &scope,
            where_expr,
            // `WHERE` is the boundary where SQL's three-valued logic
            // collapses to two: a predicate whose truth is unknown
            // excludes the row exactly like a false one.
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

    // Compute every genuine-expression sort key into its own register,
    // appended after the schema-column block. A key's final register
    // need not be the highest one its expression allocates (e.g. `CASE`
    // allocates its destination before its branches), so the record's
    // span is widened to `reg`'s post-compile watermark rather than
    // trusting the last returned register — any intervening temporary
    // just becomes an unread extra field.
    let mut sort_keys = Vec::with_capacity(order_by_plans.len());
    for plan in order_by_plans {
        let index = match &plan.target {
            OrderByTarget::Column(idx) => *idx,
            OrderByTarget::Expr(expr) => {
                let r = compile_value(em, reg, &scope, expr)?;
                usize::try_from(r.saturating_sub(first)).unwrap_or(0)
            }
        };
        sort_keys.push(SortKeyColumn {
            index,
            descending: plan.descending,
            collation: plan.collation,
            nulls_first: plan.nulls_first,
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

    // Pass 2: iterate the sorted buffer, re-deriving the schema's full
    // column tuple from each sorted record via an `OpenPseudo` cursor,
    // then apply DISTINCT/LIMIT/OFFSET and emit result columns exactly
    // as the direct-scan path does, reading from `cursors.pseudo`
    // instead of `cursors.table`.
    em.place(sort_step);
    let sort_addr = em.emit(Instruction::new(Opcode::SorterSort, cursors.sort, 0, 0));
    em.patch_p2(sort_addr, end_label);

    let sorted_loop = em.new_label();
    em.place(sorted_loop);
    let sorter_data_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::SorterData,
        cursors.sort,
        sorter_data_reg,
        0,
    ));
    // Re-opened every iteration rather than opened once before the loop
    // with `sorter_data_reg` merely updated: `cursor.rs`'s pseudo-cursor
    // is a cheap, idempotent register-pointer rebind (no allocation or
    // I/O), and this mirrors SQLite's own per-row `OpenPseudo` re-open
    // when the underlying data register changes each iteration.
    em.emit(Instruction::new(
        Opcode::OpenPseudo,
        cursors.pseudo,
        sorter_data_reg,
        0,
    ));

    let row_skip = em.new_label();
    emit_distinct_guard(
        em,
        reg,
        select,
        schema,
        cursors.pseudo,
        true,
        cursors.distinct,
        row_skip,
        catalog,
    )?;
    if let Some(limit) = &limit {
        emit_offset_guard(em, limit, row_skip);
    }
    if let Some(limit) = &limit {
        emit_limit_guard(em, limit, end_label);
    }
    emit_row_via_sink(em, reg, select, schema, cursors.pseudo, true, catalog, sink)?;

    em.place(row_skip);
    let sorted_next = em.emit(Instruction::new(Opcode::SorterNext, cursors.sort, 0, 0));
    em.patch_p2(sorted_next, sorted_loop);
    Ok(())
}
