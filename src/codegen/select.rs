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

use crate::codegen::expr::{
    collation_of, column_index, compile_cond, compile_value, emit_column_read,
};
use crate::codegen::{CondTargets, Emitter, Label, RegAlloc, Scope, TableBinding, Target};
use crate::parser::ast::{
    BinaryOp, Distinctness, Expr, ExprKind, JoinConstraint, JoinOp, Literal, ParamKind,
    ResultColumn, Select, TableRef,
};
use crate::parser::tokenizer::Span;
use crate::schema::{rowid_alias_column, TableSchema};
use crate::vdbe::{Collation, Instruction, Opcode, Program, SortKeyColumn, P4};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodegenError {
    #[error("SELECT has no FROM clause — not supported by this V2-scope compiler")]
    NoFromClause,

    #[error("unknown column {name:?}")]
    UnknownColumn { name: String },

    /// #237: an unqualified column name in a multi-table `FROM` matched
    /// more than one joined table's schema.
    #[error("ambiguous column name: {name:?}")]
    AmbiguousColumn { name: String },

    #[error("unsupported: {reason}")]
    Unsupported { reason: String },

    /// #195: an `INSERT` row supplied a different number of values than
    /// the target column list names.
    #[error("{table} has {expected} columns but {found} values were supplied")]
    RowShapeMismatch {
        table: String,
        expected: usize,
        found: usize,
    },
}

const TABLE_CURSOR: i32 = 0;
const SORT_CURSOR: i32 = 1;
const PSEUDO_CURSOR: i32 = 2;
const DISTINCT_CURSOR: i32 = 3;

/// The scan's cursor numbers, parameterized (rather than the fixed
/// `TABLE_CURSOR`/`SORT_CURSOR`/`PSEUDO_CURSOR`/`DISTINCT_CURSOR`
/// constants) so [`compile_select_scan`] can be embedded inside another
/// statement's program (#208: `INSERT ... SELECT`) without colliding
/// with that statement's own cursor numbers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanCursors {
    pub(crate) table: i32,
    pub(crate) sort: i32,
    pub(crate) pseudo: i32,
    pub(crate) distinct: i32,
}

impl ScanCursors {
    const fn for_standalone_select() -> Self {
        Self {
            table: TABLE_CURSOR,
            sort: SORT_CURSOR,
            pseudo: PSEUDO_CURSOR,
            distinct: DISTINCT_CURSOR,
        }
    }
}

/// Compiles `select` against `schema` (the resolved `FROM` table) into
/// a `Program`. Single-table only — a `select.from` with a non-empty
/// `joins` list (#237) has more than one table to resolve schemas for,
/// which this single-`schema` signature has no way to accept; use
/// [`compile_select_joined`] instead. Subqueries in `FROM` (#238)
/// aren't represented in the AST at all yet.
pub fn compile_select(select: &Select, schema: &TableSchema) -> Result<Program, CodegenError> {
    compile_select_with_catalog(select, schema, std::slice::from_ref(schema))
}

/// [`compile_select`], plus `catalog` — the full table catalog (#238),
/// used to resolve a scalar/`IN`/`EXISTS` subquery expression's own
/// `FROM` table when it names a table other than `schema` itself.
/// `compile_select` is the common case (no cross-table subquery
/// support needed, or a subquery that only ever selects from `schema`
/// itself) and just calls through with `catalog = [schema]`.
pub fn compile_select_with_catalog(
    select: &Select,
    schema: &TableSchema,
    catalog: &[TableSchema],
) -> Result<Program, CodegenError> {
    let Some(from) = &select.from else {
        return Err(CodegenError::NoFromClause);
    };
    if !from.joins.is_empty() {
        return Err(CodegenError::Unsupported {
            reason: "this SELECT's FROM clause has a JOIN — call compile_select_joined with \
                     every joined table's schema instead of compile_select"
                .to_string(),
        });
    }

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();

    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    let cursors = ScanCursors::for_standalone_select();
    em.emit(Instruction::new(
        Opcode::OpenRead,
        cursors.table,
        i32::try_from(schema.root_page).unwrap_or(0),
        0,
    ));

    let end_label = em.new_label();
    let mut sink = |em: &mut Emitter, _reg: &mut RegAlloc, first: i32, count: i32| {
        em.emit(Instruction::new(Opcode::ResultRow, first, count, 0));
        Ok(())
    };
    compile_select_scan(
        &mut em, &mut reg, select, schema, cursors, end_label, catalog, &mut sink,
    )?;

    em.place(end_label);
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

    Ok(em.finish())
}

/// The scan/filter/project core of `compile_select`, minus the
/// `Init`/`OpenRead`/`Halt` bracketing — factored out so #208's `INSERT
/// ... SELECT` codegen can drive the same scan (with its own cursor
/// numbers and its own `OpenRead` already emitted) and substitute a
/// different per-row `sink` in place of `ResultRow`. Generic over `sink`
/// (rather than a `dyn FnMut` trait object) per this codebase's
/// qualified-subset gate (`make mvl-limit`) — no dynamic dispatch.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_select_scan<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    let order_by_plans = resolve_order_by(select, schema)?;
    if order_by_plans.is_empty() {
        compile_direct_scan(em, reg, select, schema, cursors, end_label, catalog, sink)
    } else {
        compile_sorted_scan(
            em,
            reg,
            select,
            schema,
            &order_by_plans,
            cursors,
            end_label,
            catalog,
            sink,
        )
    }
}

/// The number of columns `select` projects against `schema` — used by
/// #208's `INSERT ... SELECT` codegen to validate row shape against the
/// target column list at compile time, the same way a literal `VALUES`
/// row's length is checked.
pub(crate) fn select_result_column_count(select: &Select, schema: &TableSchema) -> usize {
    result_columns(select, schema).len()
}

/// Compiles a joined `select` (#237: `INNER`/plain `JOIN`, `LEFT
/// [OUTER] JOIN`, `CROSS JOIN`) against `schemas` — one schema per
/// table in `select.from`'s order: the first table, then each
/// `Join::table` in `select.from.joins`'s order. A classic
/// nested-loop join: `OpenRead` every cursor up front, then
/// outer-to-inner `Rewind`/`Next` (the first table outermost),
/// testing each join's `ON` condition right after entering its own
/// loop. `LEFT JOIN` additionally tracks a per-outer-row "matched"
/// flag register and, when no inner row satisfied `ON`, emits exactly
/// one row with that table's (and anything joined off of it)
/// columns forced to NULL — see [`compile_join_level`].
///
/// TODO(#237 follow-up): `ORDER BY`/`DISTINCT` combined with a JOIN
/// are rejected outright (`Unsupported`) rather than silently
/// mis-compiled — `compile_sorted_scan`/the ephemeral-index DISTINCT
/// guard are both hard-wired to a single `TableSchema`, and
/// generalizing them to a multi-table `Scope` was out of this
/// ticket's bounded scope. `WHERE`/`LIMIT`/`OFFSET`/projections
/// (including `*`/`table.*`) all work across the join.
pub fn compile_select_joined(
    select: &Select,
    schemas: &[TableSchema],
) -> Result<Program, CodegenError> {
    let Some(from) = &select.from else {
        return Err(CodegenError::NoFromClause);
    };
    let table_count = from.joins.len().saturating_add(1);
    if schemas.len() != table_count {
        return Err(CodegenError::Unsupported {
            reason: format!(
                "compile_select_joined needs one schema per FROM table ({table_count} tables, \
                 {} schemas given)",
                schemas.len()
            ),
        });
    }
    if !select.order_by.is_empty() {
        return Err(CodegenError::Unsupported {
            reason: "ORDER BY combined with a JOIN is not yet supported".to_string(),
        });
    }
    if matches!(select.distinct, Some(Distinctness::Distinct)) {
        return Err(CodegenError::Unsupported {
            reason: "DISTINCT combined with a JOIN is not yet supported".to_string(),
        });
    }

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();

    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    let table_refs: Vec<&TableRef> = std::iter::once(&from.first)
        .chain(from.joins.iter().map(|j| &j.table))
        .collect();
    let mut bindings = Vec::with_capacity(schemas.len());
    for (i, (table_ref, schema)) in table_refs.iter().zip(schemas.iter()).enumerate() {
        let cursor = i32::try_from(i).unwrap_or(0);
        em.emit(Instruction::new(
            Opcode::OpenRead,
            cursor,
            i32::try_from(schema.root_page).unwrap_or(0),
            0,
        ));
        bindings.push(TableBinding {
            alias: table_ref.alias.clone(),
            name: table_ref.name.clone(),
            schema: schema.clone(),
            cursor,
            forced_null: false,
        });
    }

    let ops: Vec<JoinOp> = from.joins.iter().map(|j| j.op).collect();
    let constraints: Vec<Option<Expr>> = from
        .joins
        .iter()
        .map(|j| j.constraint.as_ref().map(|JoinConstraint::On(e)| e.clone()))
        .collect();

    let full_scope = Scope {
        tables: bindings.clone(),
        catalog: schemas.to_vec(),
        outer: None,
    };
    let limit = compile_limit_setup(&mut em, &mut reg, &full_scope, select)?;

    let end_label = em.new_label();
    let mut sink = |em: &mut Emitter, _reg: &mut RegAlloc, first: i32, count: i32| {
        em.emit(Instruction::new(Opcode::ResultRow, first, count, 0));
        Ok(())
    };
    let mut null_mask = vec![false; bindings.len()];
    compile_join_level(
        &mut em,
        &mut reg,
        select,
        &bindings,
        &ops,
        &constraints,
        &mut null_mask,
        0,
        end_label,
        limit.as_ref(),
        schemas,
        &mut sink,
    )?;

    em.place(end_label);
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

    Ok(em.finish())
}

/// Builds the [`Scope`] a join-tree node sees at compile time: every
/// binding as-is, except that `null_mask[i]` (LEFT JOIN's no-match
/// branch, see [`compile_join_level`]) forces binding `i`'s
/// `forced_null` flag on for this recursion branch only — the shared
/// `bindings` vec itself is never mutated.
fn join_scope(bindings: &[TableBinding], null_mask: &[bool], catalog: &[TableSchema]) -> Scope {
    Scope {
        tables: bindings
            .iter()
            .zip(null_mask.iter())
            .map(|(b, &forced_null)| TableBinding {
                alias: b.alias.clone(),
                name: b.name.clone(),
                schema: b.schema.clone(),
                cursor: b.cursor,
                forced_null: forced_null || b.forced_null,
            })
            .collect(),
        catalog: catalog.to_vec(),
        outer: None,
    }
}

/// Recursively emits the nested-loop join, one table per recursion
/// level (`level` indexes into `bindings`/`ops`/`constraints`, where
/// `ops[i]`/`constraints[i]` belong to the join that brought in
/// `bindings[i + 1]`). `level == bindings.len()` is the innermost
/// point — every table's cursor is positioned on a candidate
/// combination, so this is where `WHERE`, `LIMIT`/`OFFSET`, and the
/// result-column projection all compile, via [`emit_join_row`].
///
/// `LEFT JOIN` (`ops[level - 1] == Left`) wraps its own `Rewind`/`Next`
/// loop with a `matched` flag register: cleared before the loop,
/// set to 1 the first time `ON` holds for some inner-side row (which
/// also fires deeper recursion for that row normally), and tested
/// with `IfNot` right after the loop exits — if it's still 0, the
/// join recurses exactly once more with `null_mask[level]` set,
/// which (per [`join_scope`]) makes every reference to this table's
/// columns — including from any join further to the right — compile
/// to a NULL literal instead of a real `Column`/`Rowid` read, so a
/// non-matching left-side row still contributes exactly one
/// null-extended output row (SQL's `LEFT JOIN` semantics), and
/// anything joined *onto* this null-extended table sees a fully
/// consistent all-NULL row for it rather than a live but
/// out-of-position cursor read.
#[allow(clippy::too_many_arguments)]
fn compile_join_level<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    bindings: &[TableBinding],
    ops: &[JoinOp],
    constraints: &[Option<Expr>],
    null_mask: &mut Vec<bool>,
    level: usize,
    end_label: Label,
    limit: Option<&LimitState>,
    catalog: &[TableSchema],
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    if level == bindings.len() {
        let scope = join_scope(bindings, null_mask, catalog);
        let row_skip = em.new_label();
        if let Some(where_expr) = &select.where_clause {
            compile_cond(
                em,
                reg,
                &scope,
                where_expr,
                CondTargets::null_is_false(Target::Fallthrough, Target::Jump(row_skip)),
            )?;
        }
        if let Some(limit) = limit {
            emit_offset_guard(em, limit, row_skip);
        }
        emit_join_row(em, reg, select, &scope, sink)?;
        if let Some(limit) = limit {
            emit_limit_guard(em, limit, end_label);
        }
        em.place(row_skip);
        return Ok(());
    }

    let Some(binding) = bindings.get(level) else {
        return Err(CodegenError::Unsupported {
            reason: "join level out of range".to_string(),
        });
    };
    let cursor = binding.cursor;
    let prev_level = level.checked_sub(1);
    let is_left = prev_level.and_then(|i| ops.get(i)) == Some(&JoinOp::Left);
    let on_expr = prev_level
        .and_then(|i| constraints.get(i))
        .cloned()
        .flatten();

    let matched = if is_left { Some(reg.alloc()) } else { None };
    if let Some(matched) = matched {
        em.emit(Instruction::new(Opcode::Integer, 0, matched, 0));
    }

    let rewind_end = em.new_label();
    let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, cursor, 0, 0));
    em.patch_p2(rewind_addr, rewind_end);
    let loop_start = em.new_label();
    em.place(loop_start);

    let skip = em.new_label();
    if let Some(on_expr) = &on_expr {
        let scope = join_scope(bindings, null_mask, catalog);
        compile_cond(
            em,
            reg,
            &scope,
            on_expr,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(skip)),
        )?;
    }
    if let Some(matched) = matched {
        em.emit(Instruction::new(Opcode::Integer, 1, matched, 0));
    }
    let next_level = level.saturating_add(1);
    compile_join_level(
        em,
        reg,
        select,
        bindings,
        ops,
        constraints,
        null_mask,
        next_level,
        end_label,
        limit,
        catalog,
        sink,
    )?;
    em.place(skip);
    let next_addr = em.emit(Instruction::new(Opcode::Next, cursor, 0, 0));
    em.patch_p2(next_addr, loop_start);
    em.place(rewind_end);

    if let Some(matched) = matched {
        // `matched` is still 0 iff no inner-side row satisfied `ON` —
        // emit exactly one null-extended row for this table (and
        // anything joined off of it) in that case, then continue.
        let do_null = em.new_label();
        let after_null = em.new_label();
        let addr = em.emit(Instruction::new(Opcode::IfNot, matched, 0, 0));
        em.patch_p2(addr, do_null);
        em.goto(after_null);

        em.place(do_null);
        if let Some(slot) = null_mask.get_mut(level) {
            *slot = true;
        }
        compile_join_level(
            em,
            reg,
            select,
            bindings,
            ops,
            constraints,
            null_mask,
            next_level,
            end_label,
            limit,
            catalog,
            sink,
        )?;
        if let Some(slot) = null_mask.get_mut(level) {
            *slot = false;
        }
        em.place(after_null);
    }
    Ok(())
}

/// Projects `select`'s result columns against `scope` (a join-aware
/// counterpart to `emit_row_via_sink`/`compile_row_values`: `*`/
/// `table.*` expand across every binding in `scope`, in FROM order,
/// rather than a single schema's columns) into a contiguous register
/// run, then hands `(first, count)` to `sink`.
fn emit_join_row<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    scope: &Scope,
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    let mut regs = Vec::new();
    for col in &select.columns {
        match col {
            ResultColumn::Star => {
                for binding in &scope.tables {
                    for idx in 0..binding.schema.columns.len() {
                        regs.push(emit_join_column(em, reg, binding, idx)?);
                    }
                }
            }
            ResultColumn::TableStar { table } => {
                let binding = scope
                    .tables
                    .iter()
                    .find(|b| b.matches_qualifier(table))
                    .ok_or_else(|| CodegenError::UnknownColumn {
                        name: format!("{table}.*"),
                    })?;
                for idx in 0..binding.schema.columns.len() {
                    regs.push(emit_join_column(em, reg, binding, idx)?);
                }
            }
            ResultColumn::Expr { expr, .. } => {
                regs.push(compile_value(em, reg, scope, expr)?);
            }
        }
    }
    let Some(&first) = regs.first() else {
        let r = reg.alloc();
        return sink(em, reg, r, 0);
    };
    for (i, r) in regs.iter().enumerate() {
        let want = first.saturating_add(i32::try_from(i).unwrap_or(i32::MAX));
        if *r != want {
            return Err(CodegenError::Unsupported {
                reason: "result columns must land in contiguous registers for MakeRecord/\
                         ResultRow (a function call or other multi-register expression mixed \
                         with other columns is not yet supported)"
                    .to_string(),
            });
        }
    }
    sink(em, reg, first, i32::try_from(regs.len()).unwrap_or(0))
}

/// Reads one `*`/`table.*`-expanded column of a joined table: NULL
/// when that binding is null-extended (LEFT JOIN's no-match branch),
/// otherwise the same `emit_column_read` every other column read in
/// this crate goes through (rowid-alias-aware, etc.).
fn emit_join_column(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    binding: &TableBinding,
    idx: usize,
) -> Result<i32, CodegenError> {
    let r = reg.alloc();
    if binding.forced_null {
        em.emit(Instruction::new(Opcode::Null, 0, r, 0));
    } else {
        emit_column_read(em, &binding.schema, binding.cursor, idx, r)?;
    }
    Ok(r)
}

/// Where an ORDER BY term's sort key comes from: a raw table column
/// (known schema index, always present in the sorter's row tuple), or
/// a genuine expression that must be computed into its own register
/// and appended to that tuple — its position within the record isn't
/// known until `compile_sorted_scan` actually allocates registers.
#[derive(Debug, Clone)]
enum OrderByTarget {
    Column(usize),
    Expr(Expr),
}

struct OrderByPlan {
    target: OrderByTarget,
    descending: bool,
    collation: Collation,
    nulls_first: bool,
}

fn resolve_order_by(
    select: &Select,
    schema: &TableSchema,
) -> Result<Vec<OrderByPlan>, CodegenError> {
    let mut plans = Vec::with_capacity(select.order_by.len());
    for term in &select.order_by {
        let base_expr = strip_collate(&term.expr);
        let target = resolve_order_by_target(base_expr, select, schema)?;
        let descending = term.desc.unwrap_or(false);
        // No NULLS clause defaults to NULLS FIRST for ASC, NULLS LAST for
        // DESC (SQLite's default, matching this compiler's prior
        // behavior); an explicit clause overrides that per direction.
        let nulls_first = term
            .nulls_last
            .map_or(!descending, |nulls_last| !nulls_last);
        plans.push(OrderByPlan {
            target,
            descending,
            collation: collation_of(&term.expr).unwrap_or(Collation::Binary),
            nulls_first,
        });
    }
    Ok(plans)
}

/// Unwraps `expr COLLATE name` (and surrounding parens) down to the
/// expression the ordering is actually keyed on; the collation itself
/// is read separately via `collation_of`.
fn strip_collate(expr: &Expr) -> &Expr {
    match &expr.kind {
        ExprKind::Collate { expr: inner, .. } | ExprKind::Paren(inner) => strip_collate(inner),
        _ => expr,
    }
}

/// One result column as seen by ORDER BY ordinal/alias resolution: its
/// full expression (so an ordinal/alias resolving to a computed
/// expression can still become an `OrderByTarget::Expr`) and its `AS`
/// alias, if any. `*`/`table.*` expand against `schema` the same way
/// `result_columns` does, since this compiler is single-table (V2
/// scope).
struct OrderByEntry {
    expr: Expr,
    alias: Option<String>,
}

/// A dummy span for expressions synthesized during `*`/`table.*`
/// expansion — not sourced from any actual token, so never used for
/// error reporting.
const SYNTHETIC_SPAN: Span = Span {
    line: 0,
    column: 0,
    offset: 0,
    len: 0,
};

fn order_by_entries(select: &Select, schema: &TableSchema) -> Vec<OrderByEntry> {
    let mut out = Vec::new();
    for col in &select.columns {
        match col {
            ResultColumn::Star | ResultColumn::TableStar { .. } => {
                for name in &schema.columns {
                    out.push(OrderByEntry {
                        expr: Expr {
                            kind: ExprKind::Column {
                                table: None,
                                catalog: None,
                                name: name.clone(),
                            },
                            span: SYNTHETIC_SPAN,
                        },
                        alias: None,
                    });
                }
            }
            ResultColumn::Expr { expr, alias } => out.push(OrderByEntry {
                expr: expr.clone(),
                alias: alias.clone(),
            }),
        }
    }
    out
}

/// Resolves a result-column expression to its `OrderByTarget`: a bare
/// unqualified column becomes a direct schema index (already present
/// in the sorter's row tuple), anything else becomes a computed
/// expression that `compile_sorted_scan` appends to that tuple.
fn order_by_target_for_expr(
    expr: &Expr,
    schema: &TableSchema,
) -> Result<OrderByTarget, CodegenError> {
    match &expr.kind {
        ExprKind::Column {
            table: None, name, ..
        } => column_index(schema, name)
            .map(OrderByTarget::Column)
            .ok_or_else(|| CodegenError::UnknownColumn { name: name.clone() }),
        _ => Ok(OrderByTarget::Expr(expr.clone())),
    }
}

fn resolve_order_by_target(
    expr: &Expr,
    select: &Select,
    schema: &TableSchema,
) -> Result<OrderByTarget, CodegenError> {
    match &expr.kind {
        ExprKind::Literal(Literal::Integer(n)) => {
            let entries = order_by_entries(select, schema);
            let zero_based = usize::try_from(*n)
                .ok()
                .and_then(|ordinal| ordinal.checked_sub(1));
            let entry = zero_based
                .and_then(|zero_based| entries.get(zero_based))
                .ok_or_else(|| CodegenError::Unsupported {
                    reason: format!(
                        "ORDER BY position {n} is out of range for a {}-column result set",
                        entries.len()
                    ),
                })?;
            order_by_target_for_expr(&entry.expr, schema)
        }
        ExprKind::Column {
            table: None, name, ..
        } => {
            // Result-column aliases take precedence over table columns
            // for ORDER BY (unlike WHERE, where aliases aren't visible
            // at all).
            let entries = order_by_entries(select, schema);
            if let Some(entry) = entries
                .iter()
                .find(|e| e.alias.as_deref() == Some(name.as_str()))
            {
                return order_by_target_for_expr(&entry.expr, schema);
            }
            column_index(schema, name)
                .map(OrderByTarget::Column)
                .ok_or_else(|| CodegenError::UnknownColumn { name: name.clone() })
        }
        _ => order_by_target_for_expr(expr, schema),
    }
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
    pseudo: bool,
    catalog: &[TableSchema],
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
                if pseudo && rowid_alias_column(schema) == Some(idx) {
                    // `cursor` is a post-`ORDER BY` `OpenPseudo` re-read
                    // of an already-materialized record (see
                    // `compile_sorted_scan`'s pass 1), not a live table
                    // cursor — there is no rowid to fetch via
                    // `Opcode::Rowid` (it isn't a table cursor at all).
                    // Pass 1 built this record via `emit_column_read`
                    // against the *real* cursor, which already resolved
                    // the rowid alias into an ordinary field at this
                    // same position — so a plain `Column` read recovers
                    // it here.
                    em.emit(Instruction::new(
                        Opcode::Column,
                        cursor,
                        i32::try_from(idx).map_err(|_| CodegenError::Unsupported {
                            reason: format!("column index {idx} does not fit in a P2 operand"),
                        })?,
                        r,
                    ));
                } else {
                    // Must go through `emit_column_read`, not a bare
                    // `Column`: this is the `*` / `tbl.*` expansion path, and
                    // an `INTEGER PRIMARY KEY` column is a NULL placeholder
                    // in the record. Emitting `Column` here is why
                    // `SELECT * FROM t` answered NULL for the rowid alias
                    // while `SELECT id FROM t` (which routes through
                    // `compile_value`) answered correctly.
                    emit_column_read(em, schema, cursor, idx, r)?;
                }
                r
            }
            ResultColumnPlan::Expr(expr) => {
                // A bare `name`/`tbl.name` reference — e.g. plain
                // `SELECT id FROM t ORDER BY id` — compiles as an `Expr`
                // here, not the `Column` variant above (that one is
                // reserved for `*`/`tbl.*` expansion), so it needs the
                // same pseudo-cursor rowid-alias special case: `Rowid`
                // only works against a real table cursor, and `cursor`
                // here may be the post-`ORDER BY` pseudo cursor instead.
                // A compound expression that merely *references* the
                // rowid alias (`id + 1`) isn't covered by this — falls
                // through to `compile_value`, matching this crate's
                // existing register-reuse limitations for compound
                // result-column expressions.
                if let ExprKind::Column {
                    name,
                    table: None,
                    catalog: None,
                } = &expr.kind
                {
                    let pseudo_rowid_idx = pseudo
                        .then(|| column_index(schema, name))
                        .flatten()
                        .filter(|idx| rowid_alias_column(schema) == Some(*idx));
                    if let Some(idx) = pseudo_rowid_idx {
                        let r = reg.alloc();
                        em.emit(Instruction::new(
                            Opcode::Column,
                            cursor,
                            i32::try_from(idx).map_err(|_| CodegenError::Unsupported {
                                reason: format!("column index {idx} does not fit in a P2 operand"),
                            })?,
                            r,
                        ));
                        r
                    } else {
                        compile_value(
                            em,
                            reg,
                            &Scope::single(schema, cursor).with_catalog(catalog.to_vec()),
                            expr,
                        )?
                    }
                } else {
                    compile_value(
                        em,
                        reg,
                        &Scope::single(schema, cursor).with_catalog(catalog.to_vec()),
                        expr,
                    )?
                }
            }
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

/// Computes each result column into a contiguous register run, then
/// hands `(first, count)` to `sink` — in place of always emitting
/// `ResultRow`, so this same call site works for `compile_select`
/// (whose sink emits `ResultRow`) and #208's `INSERT ... SELECT` (whose
/// sink feeds the row into `insert.rs`'s per-row write path).
#[allow(clippy::too_many_arguments)]
fn emit_row_via_sink<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursor: i32,
    pseudo: bool,
    catalog: &[TableSchema],
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    let cols = result_columns(select, schema);
    let (first, count) = compile_row_values(em, reg, schema, &cols, cursor, pseudo, catalog)?;
    sink(em, reg, first, i32::try_from(count).unwrap_or(0))
}

#[allow(clippy::too_many_arguments)]
fn emit_distinct_guard(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursor: i32,
    pseudo: bool,
    distinct_cursor: i32,
    skip_label: Label,
    catalog: &[TableSchema],
) -> Result<(), CodegenError> {
    if !matches!(select.distinct, Some(Distinctness::Distinct)) {
        return Ok(());
    }
    let cols = result_columns(select, schema);
    let (first, count) = compile_row_values(em, reg, schema, &cols, cursor, pseudo, catalog)?;
    let count = i32::try_from(count).unwrap_or(0);
    let addr = em.emit(Instruction::with_p4(
        Opcode::Found,
        distinct_cursor,
        0,
        first,
        P4::Int(i64::from(count)),
    ));
    em.patch_p2(addr, skip_label);
    em.emit(Instruction::with_p4(
        Opcode::IdxInsert,
        distinct_cursor,
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
fn top_level_equality_operands(expr: &Expr) -> Option<(&Expr, &Expr)> {
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

fn is_rowid_reference(schema: &TableSchema, expr: &Expr) -> bool {
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
fn try_compile_rowid_seek<F>(
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
    emit_row_via_sink(em, reg, select, schema, cursors.table, false, catalog, sink)?;
    if let Some(limit) = &limit {
        emit_limit_guard(em, limit, end_label);
    }
    em.place(row_skip);
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn compile_direct_scan<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schema: &TableSchema,
    cursors: ScanCursors,
    end_label: Label,
    catalog: &[TableSchema],
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    if try_compile_rowid_seek(em, reg, select, schema, cursors, end_label, catalog, sink)? {
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
    emit_row_via_sink(em, reg, select, schema, cursors.table, false, catalog, sink)?;
    if let Some(limit) = &limit {
        emit_limit_guard(em, limit, end_label);
    }

    em.place(row_skip);
    let next_addr = em.emit(Instruction::new(Opcode::Next, cursors.table, 0, 0));
    em.patch_p2(next_addr, loop_start);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compile_sorted_scan<F>(
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
    // The sort-key descriptor (which register each term reads) isn't
    // known until pass 1 below actually allocates the computed-expression
    // registers, so `SorterOpen` is emitted with a placeholder P4 and
    // patched once that layout is known — it must still precede the scan
    // loop in program order.
    let sorter_open_addr = em.emit(Instruction::with_p4(
        Opcode::SorterOpen,
        cursors.sort,
        0,
        0,
        P4::None,
    ));

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

    let scope = Scope::single(schema, cursors.table).with_catalog(catalog.to_vec());
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

    let limit = compile_limit_setup(em, reg, &scope, select)?;

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
    emit_row_via_sink(em, reg, select, schema, cursors.pseudo, true, catalog, sink)?;
    if let Some(limit) = &limit {
        emit_limit_guard(em, limit, end_label);
    }

    em.place(row_skip);
    let sorted_next = em.emit(Instruction::new(Opcode::SorterNext, cursors.sort, 0, 0));
    em.patch_p2(sorted_next, sorted_loop);
    Ok(())
}
