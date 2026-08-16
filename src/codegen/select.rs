//! `Select` AST -> `Program` compilation (spec 009, Requirement 11's
//! surrounding statement shape): `Init -> OpenRead -> Rewind -> [WHERE
//! test, result columns, ResultRow] -> Next -> Halt`, with ORDER BY
//! wired through the sorter opcodes, LIMIT/OFFSET as independent
//! `IfPos`/`DecrJumpZero` counters, and DISTINCT via the in-memory
//! ephemeral index — mirroring `tests/vdbe/cursor_sorter_test.rs`'s
//! hand-assembled shapes.
//!
//! Known simplification: LIMIT/OFFSET compile to two independent
//! counters (`IfPos` to skip the first OFFSET matching rows, then
//! `DecrJumpZero` to stop after LIMIT rows) rather than the single
//! combined budget register `OffsetLimit` computes — `OffsetLimit`
//! itself was already implemented and tested by #89; this ticket just
//! doesn't happen to need it for a correct LIMIT/OFFSET shape.

use thiserror::Error;

use crate::codegen::expr::{column_index, compile_cond, compile_value, emit_column_read};
use crate::codegen::{CondTargets, Emitter, Label, RegAlloc, Target};
use crate::parser::ast::{Distinctness, Expr, ExprKind, ResultColumn, Select};
use crate::schema::TableSchema;
use crate::vdbe::{Collation, Instruction, Opcode, Program, SortKeyColumn, P4};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodegenError {
    #[error("SELECT has no FROM clause — not supported by this V2-scope compiler")]
    NoFromClause,

    #[error("unknown column {name:?}")]
    UnknownColumn { name: String },

    #[error("unsupported: {reason}")]
    Unsupported { reason: String },
}

const TABLE_CURSOR: i32 = 0;
const SORT_CURSOR: i32 = 1;
const PSEUDO_CURSOR: i32 = 2;
const DISTINCT_CURSOR: i32 = 3;

/// Compiles `select` against `schema` (the resolved `FROM` table) into
/// a `Program`. Single-table V2 scope only — no joins/subqueries.
pub fn compile_select(select: &Select, schema: &TableSchema) -> Result<Program, CodegenError> {
    if select.from.is_none() {
        return Err(CodegenError::NoFromClause);
    }

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();

    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    em.emit(Instruction::new(
        Opcode::OpenRead,
        TABLE_CURSOR,
        i32::try_from(schema.root_page).unwrap_or(0),
        0,
    ));

    let end_label = em.new_label();
    let order_by_keys = resolve_order_by(select, schema)?;

    if order_by_keys.is_empty() {
        compile_direct_scan(&mut em, &mut reg, select, schema, end_label)?;
    } else {
        compile_sorted_scan(&mut em, &mut reg, select, schema, &order_by_keys, end_label)?;
    }

    em.place(end_label);
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

    Ok(em.finish())
}

fn resolve_order_by(
    select: &Select,
    schema: &TableSchema,
) -> Result<Vec<SortKeyColumn>, CodegenError> {
    let mut keys = Vec::with_capacity(select.order_by.len());
    for term in &select.order_by {
        let ExprKind::Column { name, .. } = &term.expr.kind else {
            return Err(CodegenError::Unsupported {
                reason: "ORDER BY only supports plain column references in this V2-scope compiler"
                    .to_string(),
            });
        };
        let idx = column_index(schema, name)
            .ok_or_else(|| CodegenError::UnknownColumn { name: name.clone() })?;
        keys.push(SortKeyColumn {
            index: idx,
            descending: term.desc.unwrap_or(false),
            collation: Collation::Binary,
        });
    }
    Ok(keys)
}

enum ResultColumnPlan {
    Column(String),
    Expr(Expr),
}

fn result_columns(select: &Select, schema: &TableSchema) -> Vec<ResultColumnPlan> {
    let mut out = Vec::new();
    for col in &select.columns {
        match col {
            ResultColumn::Star | ResultColumn::TableStar { .. } => {
                for name in &schema.columns {
                    out.push(ResultColumnPlan::Column(name.clone()));
                }
            }
            ResultColumn::Expr { expr, .. } => out.push(ResultColumnPlan::Expr(expr.clone())),
        }
    }
    out
}

/// Compiles each result column into a contiguous register range
/// starting at a freshly allocated register, returning `(first, count)`.
fn compile_row_values(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    schema: &TableSchema,
    cols: &[ResultColumnPlan],
    cursor: i32,
) -> Result<(i32, usize), CodegenError> {
    // Each column is compiled into whatever register the bump
    // allocator hands out next (not pre-reserved), since a compound
    // expression (e.g. CASE) may itself allocate temporaries before
    // settling on its final result register. `MakeRecord`/`ResultRow`
    // need a contiguous run, so columns are only safe to compile
    // straight through when every one of them is a "simple" shape that
    // allocates exactly its own destination register and nothing more
    // (`Column`, a bare literal, or a plain `Column` expr) — true for
    // the whole V2 corpus's result-column shapes. A future ticket
    // needs a MOVE-style opcode to relax this for arbitrary compound
    // expressions mixed with other columns.
    let mut regs = Vec::with_capacity(cols.len());
    for col in cols {
        let r = match col {
            ResultColumnPlan::Column(name) => {
                let idx =
                    column_index(schema, name).ok_or_else(|| CodegenError::UnknownColumn {
                        name: (*name).to_string(),
                    })?;
                let r = reg.alloc();
                // Must go through `emit_column_read`, not a bare
                // `Column`: this is the `*` / `tbl.*` expansion path, and
                // an `INTEGER PRIMARY KEY` column is a NULL placeholder
                // in the record. Emitting `Column` here is why
                // `SELECT * FROM t` answered NULL for the rowid alias
                // while `SELECT id FROM t` (which routes through
                // `compile_value`) answered correctly.
                emit_column_read(em, schema, cursor, idx, r)?;
                r
            }
            ResultColumnPlan::Expr(expr) => compile_value(em, reg, schema, cursor, expr)?,
        };
        regs.push(r);
    }
    if cols.is_empty() {
        return Ok((reg.alloc(), 0));
    }
    let Some(&first) = regs.first() else {
        return Ok((reg.alloc(), 0));
    };
    for (i, r) in regs.iter().enumerate() {
        let want = first.saturating_add(i32::try_from(i).unwrap_or(i32::MAX));
        if *r != want {
            return Err(CodegenError::Unsupported {
                reason:
                    "result columns must land in contiguous registers for MakeRecord/ResultRow \
                         (a function call or other multi-register expression mixed with other \
                         columns is not yet supported)"
                        .to_string(),
            });
        }
    }
    Ok((first, cols.len()))
}

fn emit_result_row(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursor: i32,
) -> Result<(), CodegenError> {
    let cols = result_columns(select, schema);
    let (first, count) = compile_row_values(em, reg, schema, &cols, cursor)?;
    em.emit(Instruction::new(
        Opcode::ResultRow,
        first,
        i32::try_from(count).unwrap_or(0),
        0,
    ));
    Ok(())
}

fn emit_distinct_guard(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursor: i32,
    skip_label: Label,
) -> Result<(), CodegenError> {
    if !matches!(select.distinct, Some(Distinctness::Distinct)) {
        return Ok(());
    }
    let cols = result_columns(select, schema);
    let (first, count) = compile_row_values(em, reg, schema, &cols, cursor)?;
    let count = i32::try_from(count).unwrap_or(0);
    let addr = em.emit(Instruction::with_p4(
        Opcode::Found,
        DISTINCT_CURSOR,
        0,
        first,
        P4::Int(i64::from(count)),
    ));
    em.patch_p2(addr, skip_label);
    em.emit(Instruction::with_p4(
        Opcode::IdxInsert,
        DISTINCT_CURSOR,
        first,
        0,
        P4::Int(i64::from(count)),
    ));
    Ok(())
}

/// LIMIT/OFFSET counters, set up once before the scan loop starts.
struct LimitState {
    offset_reg: Option<i32>,
    limit_reg: Option<i32>,
}

fn compile_limit_setup(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    schema: &TableSchema,
    select: &Select,
) -> Result<Option<LimitState>, CodegenError> {
    let Some(limit) = &select.limit else {
        return Ok(None);
    };
    let limit_reg = compile_value(em, reg, schema, TABLE_CURSOR, &limit.limit)?;
    let offset_reg = match &limit.offset {
        Some(offset_expr) => Some(compile_value(em, reg, schema, TABLE_CURSOR, offset_expr)?),
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
fn emit_offset_guard(em: &mut Emitter, limit: &LimitState, row_skip: Label) {
    if let Some(offset_reg) = limit.offset_reg {
        let addr = em.emit(Instruction::new(Opcode::IfPos, offset_reg, 0, 1));
        em.patch_p2(addr, row_skip);
    }
}

/// Emits the LIMIT stop-guard (jumping to `end_label` once `limit_reg`
/// reaches zero) — call once per row actually emitted.
fn emit_limit_guard(em: &mut Emitter, limit: &LimitState, end_label: Label) {
    if let Some(limit_reg) = limit.limit_reg {
        let addr = em.emit(Instruction::new(Opcode::DecrJumpZero, limit_reg, 0, 0));
        em.patch_p2(addr, end_label);
    }
}

fn compile_direct_scan(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    end_label: Label,
) -> Result<(), CodegenError> {
    if matches!(select.distinct, Some(Distinctness::Distinct)) {
        em.emit(Instruction::new(
            Opcode::OpenEphemeral,
            DISTINCT_CURSOR,
            0,
            0,
        ));
    }
    let limit = compile_limit_setup(em, reg, schema, select)?;

    let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, TABLE_CURSOR, 0, 0));
    em.patch_p2(rewind_addr, end_label);
    let loop_start = em.new_label();
    em.place(loop_start);

    let row_skip = em.new_label();
    if let Some(where_expr) = &select.where_clause {
        compile_cond(
            em,
            reg,
            schema,
            TABLE_CURSOR,
            where_expr,
            // `WHERE` is the boundary where SQL's three-valued logic
            // collapses to two: a predicate whose truth is unknown
            // excludes the row exactly like a false one.
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(row_skip)),
        )?;
    }
    emit_distinct_guard(em, reg, select, schema, TABLE_CURSOR, row_skip)?;
    if let Some(limit) = &limit {
        emit_offset_guard(em, limit, row_skip);
    }
    emit_result_row(em, reg, select, schema, TABLE_CURSOR)?;
    if let Some(limit) = &limit {
        emit_limit_guard(em, limit, end_label);
    }

    em.place(row_skip);
    let next_addr = em.emit(Instruction::new(Opcode::Next, TABLE_CURSOR, 0, 0));
    em.patch_p2(next_addr, loop_start);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compile_sorted_scan(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    order_by_keys: &[SortKeyColumn],
    end_label: Label,
) -> Result<(), CodegenError> {
    if matches!(select.distinct, Some(Distinctness::Distinct)) {
        em.emit(Instruction::new(
            Opcode::OpenEphemeral,
            DISTINCT_CURSOR,
            0,
            0,
        ));
    }
    em.emit(Instruction::with_p4(
        Opcode::SorterOpen,
        SORT_CURSOR,
        0,
        0,
        P4::SortKey(order_by_keys.to_vec()),
    ));

    // Pass 1: buffer every matching row's full column tuple into the
    // sorter, WHERE-filtered but pre-DISTINCT/LIMIT (those apply on
    // the sorted output, matching SQLite's own ORDER BY pipeline
    // shape).
    let scan_rewind = em.emit(Instruction::new(Opcode::Rewind, TABLE_CURSOR, 0, 0));
    let sort_step = em.new_label();
    em.patch_p2(scan_rewind, sort_step);
    let scan_loop = em.new_label();
    em.place(scan_loop);

    let scan_skip = em.new_label();
    if let Some(where_expr) = &select.where_clause {
        compile_cond(
            em,
            reg,
            schema,
            TABLE_CURSOR,
            where_expr,
            // `WHERE` is the boundary where SQL's three-valued logic
            // collapses to two: a predicate whose truth is unknown
            // excludes the row exactly like a false one.
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(scan_skip)),
        )?;
    }
    let (first, count) = compile_row_values(
        em,
        reg,
        schema,
        &schema
            .columns
            .iter()
            .map(|c| ResultColumnPlan::Column(c.clone()))
            .collect::<Vec<_>>(),
        TABLE_CURSOR,
    )?;
    let record_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::MakeRecord,
        first,
        i32::try_from(count).unwrap_or(0),
        record_reg,
    ));
    em.emit(Instruction::new(
        Opcode::SorterInsert,
        SORT_CURSOR,
        record_reg,
        0,
    ));

    em.place(scan_skip);
    let scan_next = em.emit(Instruction::new(Opcode::Next, TABLE_CURSOR, 0, 0));
    em.patch_p2(scan_next, scan_loop);

    // Pass 2: iterate the sorted buffer, re-deriving the schema's full
    // column tuple from each sorted record via an `OpenPseudo` cursor,
    // then apply DISTINCT/LIMIT/OFFSET and emit result columns exactly
    // as the direct-scan path does, reading from `PSEUDO_CURSOR`
    // instead of `TABLE_CURSOR`.
    em.place(sort_step);
    let sort_addr = em.emit(Instruction::new(Opcode::SorterSort, SORT_CURSOR, 0, 0));
    em.patch_p2(sort_addr, end_label);

    let limit = compile_limit_setup(em, reg, schema, select)?;

    let sorted_loop = em.new_label();
    em.place(sorted_loop);
    let sorter_data_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::SorterData,
        SORT_CURSOR,
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
        PSEUDO_CURSOR,
        sorter_data_reg,
        0,
    ));

    let row_skip = em.new_label();
    emit_distinct_guard(em, reg, select, schema, PSEUDO_CURSOR, row_skip)?;
    if let Some(limit) = &limit {
        emit_offset_guard(em, limit, row_skip);
    }
    emit_result_row(em, reg, select, schema, PSEUDO_CURSOR)?;
    if let Some(limit) = &limit {
        emit_limit_guard(em, limit, end_label);
    }

    em.place(row_skip);
    let sorted_next = em.emit(Instruction::new(Opcode::SorterNext, SORT_CURSOR, 0, 0));
    em.patch_p2(sorted_next, sorted_loop);
    Ok(())
}
