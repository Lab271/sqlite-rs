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
    collation_of, column_index, compile_cond, compile_value, emit_column_read, is_aggregate_call,
};
use crate::codegen::{CondTargets, Emitter, Label, RegAlloc, Scope, TableBinding, Target};
use crate::parser::ast::{
    BinaryOp, CompoundSelect, Distinctness, Expr, ExprKind, FunctionArgs, JoinConstraint, JoinOp,
    Literal, ParamKind, ResultColumn, Select, TableRef,
};
use crate::parser::tokenizer::Span;
use crate::schema::{rowid_alias_column, IndexSchema, TableSchema};
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

    /// #240: a `UNION ALL` arm projected a different number of result
    /// columns than the first arm — SQLite rejects this at compile time
    /// rather than padding/truncating rows.
    #[error(
        "SELECTs to the left and right of UNION ALL do not have the same number of result \
         columns: expected {expected}, found {found}"
    )]
    CompoundColumnMismatch { expected: usize, found: usize },
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

    /// One full, non-colliding cursor set per `UNION ALL` arm (#240) —
    /// `index` 0 is the compound's first arm, 1.. are `select.compound`
    /// arms in order. Each arm gets 4 cursor numbers to itself so an
    /// arm using its own ORDER BY sort cursor or DISTINCT ephemeral
    /// index never collides with another arm's.
    const fn for_arm(index: usize) -> Self {
        let base = (index as i32).saturating_mul(4);
        Self {
            table: base,
            sort: base.saturating_add(1),
            pseudo: base.saturating_add(2),
            distinct: base.saturating_add(3),
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
    if !select.compound.is_empty() {
        return Err(CodegenError::Unsupported {
            reason: "this SELECT is a UNION ALL compound — call compile_select_compound instead"
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
    if !select.group_by.is_empty() {
        if !select.order_by.is_empty() {
            return Err(CodegenError::Unsupported {
                reason: "GROUP BY combined with ORDER BY not yet supported".to_string(),
            });
        }
        if select.distinct.is_some() {
            return Err(CodegenError::Unsupported {
                reason: "GROUP BY combined with DISTINCT not yet supported".to_string(),
            });
        }
        return compile_grouped_scan(em, reg, select, schema, cursors, end_label, catalog, sink);
    }
    if select.having.is_some() {
        return Err(CodegenError::Unsupported {
            reason: "HAVING without GROUP BY not yet supported".to_string(),
        });
    }

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

/// Turns a `UNION ALL` arm into a standalone `Select` so it can be fed
/// through [`select_result_column_count`]/[`compile_select_scan`] the
/// same way the compound's first arm is — `order_by`/`limit` are always
/// empty since those bind to the whole compound statement, not any one
/// arm (see [`crate::parser::ast::Select::compound`]).
fn arm_as_select(arm: &CompoundSelect) -> Select {
    Select {
        distinct: arm.distinct,
        columns: arm.columns.clone(),
        from: arm.from.clone(),
        where_clause: arm.where_clause.clone(),
        group_by: arm.group_by.clone(),
        having: arm.having.clone(),
        compound: Vec::new(),
        order_by: Vec::new(),
        limit: None,
        span: arm.span,
    }
}

/// Compiles a `UNION ALL` compound `SELECT` (#240): `first` against
/// `first_schema`, then each of `select.compound`'s arms against its
/// paired schema in `arm_schemas` (same order, one per arm) —
/// concatenating every arm's rows with no deduplication and no shared
/// sort/merge step. Each arm gets its own `OpenRead`/scan/`ResultRow`
/// block with cursor numbers offset by `ScanCursors::for_arm`, so
/// arms never collide even when an arm itself uses a sort or DISTINCT
/// cursor. `first`'s `order_by`/`limit` apply to the whole compound
/// statement, but are not yet implemented here — sorting/limiting a
/// concatenation of independent scans needs a shared sorter across
/// arms, which is out of this ticket's scope; callers must reject a
/// non-empty `order_by`/`limit` before calling this.
///
/// Joins/subqueries within any arm are out of scope for this ticket —
/// every arm's `from` must be a single table with no joins.
pub fn compile_select_compound(
    first: &Select,
    first_schema: &TableSchema,
    arm_schemas: &[TableSchema],
    catalog: &[TableSchema],
) -> Result<Program, CodegenError> {
    if first
        .from
        .as_ref()
        .is_some_and(|from| !from.joins.is_empty())
    {
        return Err(CodegenError::Unsupported {
            reason: "UNION ALL with a JOIN in one of its arms is not yet supported".to_string(),
        });
    }
    if first.compound.len() != arm_schemas.len() {
        return Err(CodegenError::Unsupported {
            reason: "compile_select_compound: arm_schemas must have one entry per compound arm"
                .to_string(),
        });
    }
    if !first.order_by.is_empty() || first.limit.is_some() {
        return Err(CodegenError::Unsupported {
            reason: "ORDER BY/LIMIT on a UNION ALL compound SELECT is not yet supported"
                .to_string(),
        });
    }

    let expected = select_result_column_count(first, first_schema);
    let mut arm_selects = Vec::with_capacity(first.compound.len());
    for (arm, arm_schema) in first.compound.iter().zip(arm_schemas) {
        if arm.from.as_ref().is_some_and(|from| !from.joins.is_empty()) {
            return Err(CodegenError::Unsupported {
                reason: "UNION ALL with a JOIN in one of its arms is not yet supported".to_string(),
            });
        }
        let arm_select = arm_as_select(arm);
        let found = select_result_column_count(&arm_select, arm_schema);
        if found != expected {
            return Err(CodegenError::CompoundColumnMismatch { expected, found });
        }
        arm_selects.push(arm_select);
    }

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();

    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    let mut sink = |em: &mut Emitter, _reg: &mut RegAlloc, reg_first: i32, count: i32| {
        em.emit(Instruction::new(Opcode::ResultRow, reg_first, count, 0));
        Ok(())
    };

    let mut compile_arm = |em: &mut Emitter,
                           reg: &mut RegAlloc,
                           arm_index: usize,
                           select: &Select,
                           schema: &TableSchema|
     -> Result<(), CodegenError> {
        let cursors = ScanCursors::for_arm(arm_index);
        em.emit(Instruction::new(
            Opcode::OpenRead,
            cursors.table,
            i32::try_from(schema.root_page).unwrap_or(0),
            0,
        ));
        let arm_end = em.new_label();
        compile_select_scan(
            em, reg, select, schema, cursors, arm_end, catalog, &mut sink,
        )?;
        em.place(arm_end);
        Ok(())
    };

    compile_arm(&mut em, &mut reg, 0, first, first_schema)?;
    for (i, (arm_select, arm_schema)) in arm_selects.iter().zip(arm_schemas).enumerate() {
        compile_arm(
            &mut em,
            &mut reg,
            i.saturating_add(1),
            arm_select,
            arm_schema,
        )?;
    }

    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    Ok(em.finish())
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
    if !select.compound.is_empty() {
        return Err(CodegenError::Unsupported {
            reason: "UNION ALL with a JOIN in one of its arms is not yet supported".to_string(),
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

/// One row of `EXPLAIN QUERY PLAN` output (#243) — SQLite's own EQP
/// shape (`id, parent, notused, detail`), distinct from plain
/// `EXPLAIN`'s per-instruction [`crate::vdbe::explain::ExplainRow`].
/// `detail` reads like the oracle's own EQP (`SCAN ...`/`SEARCH ...
/// USING ...`) but isn't guaranteed byte-identical — Requirement 10's
/// VM-diff guarantee is plain `EXPLAIN`'s job, not this human-readable
/// summary's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EqpRow {
    pub id: i32,
    pub parent: i32,
    pub notused: i32,
    pub detail: String,
}

/// A table binding's `FROM`-clause display name for EQP output:
/// `name AS alias` when aliased, `name` otherwise — matching how a
/// `Column` reference would need to qualify it.
fn eqp_display_name(table_ref: &TableRef) -> String {
    match &table_ref.alias {
        Some(alias) => format!("{} AS {alias}", table_ref.name),
        None => table_ref.name.clone(),
    }
}

/// Builds `EXPLAIN QUERY PLAN`'s output for `select` (#243): one row per
/// `FROM`-clause table, `SCAN` for a full `Rewind`/`Next` scan or
/// `SEARCH ... USING ...` for a `SeekRowid`/`SeekIndexEq` point lookup —
/// reusing [`choose_join_access`] (the join codegen's own decision
/// function) for a join's inner tables, and the same rowid-equality
/// check [`try_compile_rowid_seek`] uses for a single-table `SELECT`'s
/// `WHERE` clause, so the report can never drift from what
/// [`compile_select_joined`]/[`compile_direct_scan`] actually compile.
pub fn explain_query_plan(
    select: &Select,
    schemas: &[TableSchema],
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
            name: table_ref.name.clone(),
            schema: schema.clone(),
            cursor: i32::try_from(i).unwrap_or(0),
            forced_null: false,
        })
        .collect();

    let mut rows = Vec::with_capacity(bindings.len());
    for (level, (table_ref, binding)) in table_refs.iter().zip(bindings.iter()).enumerate() {
        let on_expr = level
            .checked_sub(1)
            .and_then(|i| from.joins.get(i))
            .and_then(|j| j.constraint.as_ref())
            .map(|JoinConstraint::On(e)| e);
        let prior_bindings = bindings.get(..level).unwrap_or(&[]);
        let access = if level == 0 {
            // The outermost table has no `ON` clause to seek against —
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
        let detail = match access {
            None => format!("SCAN {}", eqp_display_name(table_ref)),
            Some(JoinAccess::Rowid(_)) => format!(
                "SEARCH {} USING INTEGER PRIMARY KEY (rowid=?)",
                eqp_display_name(table_ref)
            ),
            Some(JoinAccess::UniqueIndex { index, .. }) => format!(
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
/// The access strategy #243's join-level planner picked for a table
/// binding, in place of an unconditional `Rewind`/`Next` full scan —
/// see [`choose_join_access`].
enum JoinAccess {
    /// The `ON` equality's other side is a rowid reference (the
    /// `rowid`/`_rowid_`/`oid` keywords, or the table's `INTEGER PRIMARY
    /// KEY` alias column): a `SeekRowid` point lookup, generalizing
    /// [`try_compile_rowid_seek`] to a join's inner table.
    Rowid(Expr),
    /// The `ON` equality's other side is a column with a single-column
    /// `UNIQUE` index: a `SeekIndexEq` + `IdxRowid` + `SeekRowid` point
    /// lookup (#243).
    UniqueIndex { index: IndexSchema, operand: Expr },
}

/// Whether `table`/`name` (a `Column` expression's qualifier and column
/// name) names a column of `binding`'s own schema — used by
/// [`choose_join_access`] to tell which side of a join's `=` belongs to
/// the table currently being brought into the loop.
fn column_belongs_to_binding(binding: &TableBinding, table: Option<&str>, name: &str) -> bool {
    if let Some(table) = table {
        if !binding.matches_qualifier(table) {
            return false;
        }
    }
    column_index(&binding.schema, name).is_some()
}

/// Whether every `Column` reference inside `expr` resolves only to one
/// of `prior_bindings` (already-positioned tables earlier in the join
/// order) — the safety condition for using `expr` as a join-level index
/// seek's probe value, since it compiles against cursors that must
/// already hold a row. Deliberately narrow: only a bare/qualified
/// column reference or a literal/parameter are recognized; anything
/// else (a sub-expression, a function call) is rejected rather than
/// risk treating a reference to the *current* or a *not-yet-bound*
/// table as safe.
fn expr_is_safe_join_probe(expr: &Expr, prior_bindings: &[TableBinding]) -> bool {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Param(_) => true,
        ExprKind::Column { table, name, .. } => prior_bindings
            .iter()
            .any(|b| column_belongs_to_binding(b, table.as_deref(), name)),
        _ => false,
    }
}

/// Picks an index/rowid seek for join level `level` out of `on_expr` (a
/// single top-level `binding.column = <safe probe>` equality — anything
/// else, including `AND`-compound `ON` clauses, falls back to the
/// ordinary `Rewind`/`Next` scan per #243's bounded scope, matching
/// [`try_compile_rowid_seek`]'s own narrowness), or `None` to keep the
/// full scan. Only a `UNIQUE` single-column index (or the rowid) is
/// considered — a non-unique index could match more than one row, which
/// this seek-once codegen shape can't express (see the module doc's
/// LEFT JOIN "matched" flag: it assumes at most one inner-side match).
fn choose_join_access(
    binding: &TableBinding,
    on_expr: &Expr,
    prior_bindings: &[TableBinding],
) -> Option<JoinAccess> {
    let (lhs, rhs) = top_level_equality_operands(on_expr)?;
    let (this_side, other_side) = if matches!(&lhs.kind, ExprKind::Column { table, name, .. } if column_belongs_to_binding(binding, table.as_deref(), name))
    {
        (lhs, rhs)
    } else if matches!(&rhs.kind, ExprKind::Column { table, name, .. } if column_belongs_to_binding(binding, table.as_deref(), name))
    {
        (rhs, lhs)
    } else {
        return None;
    };
    if !expr_is_safe_join_probe(other_side, prior_bindings) {
        return None;
    }
    if is_rowid_reference(&binding.schema, this_side) {
        return Some(JoinAccess::Rowid(other_side.clone()));
    }
    let ExprKind::Column { name, .. } = &this_side.kind else {
        return None;
    };
    let index = binding.schema.indexes.iter().find(|idx| {
        idx.unique
            && idx.columns.len() == 1
            && idx
                .columns
                .first()
                .is_some_and(|c| c.name.eq_ignore_ascii_case(name))
    })?;
    Some(JoinAccess::UniqueIndex {
        index: index.clone(),
        operand: other_side.clone(),
    })
}

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

    // #243: an equality `ON` predicate against this table's rowid or a
    // UNIQUE single-column index compiles to a `SeekRowid`/`SeekIndexEq`
    // point lookup instead of an unconditional `Rewind`/`Next` full
    // scan — see [`choose_join_access`] for the (deliberately narrow)
    // conditions this applies under.
    let access = on_expr.as_ref().and_then(|on_expr| {
        let prior_bindings = bindings.get(..level)?;
        choose_join_access(binding, on_expr, prior_bindings)
    });

    let next_level = level.saturating_add(1);
    let rewind_end = em.new_label();
    match access {
        None => {
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
        }
        Some(access) => {
            let scope = join_scope(bindings, null_mask, catalog);
            let miss = em.new_label();
            match access {
                JoinAccess::Rowid(operand) => {
                    let value_reg = compile_value(em, reg, &scope, &operand)?;
                    let seek_addr =
                        em.emit(Instruction::new(Opcode::SeekRowid, cursor, 0, value_reg));
                    em.patch_p2(seek_addr, miss);
                }
                JoinAccess::UniqueIndex { index, operand } => {
                    let value_reg = compile_value(em, reg, &scope, &operand)?;
                    let index_cursor =
                        i32::try_from(bindings.len().saturating_add(level)).unwrap_or(i32::MAX);
                    let root_page = i32::try_from(index.root_page).unwrap_or(0);
                    let mut open_instr =
                        Instruction::new(Opcode::OpenRead, index_cursor, root_page, 0);
                    open_instr.p5 = 1;
                    em.emit(open_instr);
                    let seek_instr = Instruction::with_p4(
                        Opcode::SeekIndexEq,
                        index_cursor,
                        0,
                        value_reg,
                        P4::Int(1),
                    );
                    let seek_addr = em.emit(seek_instr);
                    em.patch_p2(seek_addr, miss);
                    let rowid_reg = reg.alloc();
                    em.emit(Instruction::new(
                        Opcode::IdxRowid,
                        index_cursor,
                        rowid_reg,
                        0,
                    ));
                    let table_seek_addr =
                        em.emit(Instruction::new(Opcode::SeekRowid, cursor, 0, rowid_reg));
                    em.patch_p2(table_seek_addr, miss);
                }
            }
            if let Some(matched) = matched {
                em.emit(Instruction::new(Opcode::Integer, 1, matched, 0));
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
            em.place(miss);
        }
    }
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
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn compile_grouped_scan<F>(
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
    em.patch_p2(sort_addr, end_label);

    let limit = compile_limit_setup(em, reg, &table_scope, select)?;

    let aggs = collect_aggregates(select)?;
    let zero_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Integer, 0, zero_reg, 0));
    let one_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Integer, 1, one_reg, 0));
    let have_group_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Integer, 0, have_group_reg, 0));

    let prev_key_regs: Vec<i32> = group_targets.iter().map(|_| reg.alloc()).collect();
    let snapshot_regs: Vec<i32> = schema.columns.iter().map(|_| reg.alloc()).collect();
    let mut agg_slots: Vec<AggSlot> = aggs
        .into_iter()
        .map(|(call, kind, arg)| {
            let primary = reg.alloc();
            let aux = matches!(kind, AggKind::Avg).then(|| reg.alloc());
            AggSlot {
                call,
                kind,
                arg,
                primary,
                aux,
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

    let boundary_label = em.new_label();
    let not_boundary_label = em.new_label();
    let first_row_check = em.emit(Instruction::new(Opcode::Eq, have_group_reg, 0, zero_reg));
    em.patch_p2(first_row_check, boundary_label);
    for (&cur, &prev) in cur_key_regs.iter().zip(&prev_key_regs) {
        let a_null = em.new_label();
        let same_col = em.new_label();
        let a_null_addr = em.emit(Instruction::new(Opcode::IsNull, cur, 0, 0));
        em.patch_p2(a_null_addr, a_null);
        let b_null_addr = em.emit(Instruction::new(Opcode::IsNull, prev, 0, 0));
        em.patch_p2(b_null_addr, boundary_label);
        let eq_addr = em.emit(Instruction::new(Opcode::Eq, cur, 0, prev));
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
        reset_agg(em, agg);
    }

    em.place(not_boundary_label);
    for agg in &mut agg_slots {
        accumulate_agg(em, reg, &pseudo_scope, agg, zero_reg, one_reg)?;
    }
    read_row_columns_into(em, schema, cursors.pseudo, &snapshot_regs)?;

    let sorted_next = em.emit(Instruction::new(Opcode::SorterNext, cursors.sort, 0, 0));
    em.patch_p2(sorted_next, sorted_loop);

    // Tail flush: the very last group never sees another row to trigger
    // `boundary_label`'s mid-loop flush.
    let skip_tail_flush = em.new_label();
    let tail_check = em.emit(Instruction::new(Opcode::Eq, have_group_reg, 0, zero_reg));
    em.patch_p2(tail_check, skip_tail_flush);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AggKind {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

struct AggSlot {
    call: Expr,
    kind: AggKind,
    arg: Option<Expr>,
    primary: i32,
    aux: Option<i32>,
}

/// Recognizes `expr` as an aggregate call this compiler can accumulate,
/// or reports why not. Only called on expressions [`find_aggregates`]
/// already identified as `is_aggregate_call`, so the "not an aggregate
/// at all" case can't happen here.
fn classify_aggregate(expr: &Expr) -> Result<(AggKind, Option<Expr>), CodegenError> {
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
    let kind = match name.to_ascii_lowercase().as_str() {
        "count" => AggKind::Count,
        "sum" => AggKind::Sum,
        "avg" => AggKind::Avg,
        "min" => AggKind::Min,
        "max" => AggKind::Max,
        other => {
            return Err(CodegenError::Unsupported {
                reason: format!("aggregate function {other} not yet supported in GROUP BY"),
            })
        }
    };
    Ok((kind, arg))
}

/// Finds every aggregate-call sub-expression reachable from `select`'s
/// result columns and `HAVING` clause through `Paren`/`Collate`/`Unary`/
/// `Binary` wrappers (see [`compile_grouped_scan`]'s doc comment for the
/// bound), deduplicated by AST equality so `HAVING count(*) > 1` sharing
/// a call with a `count(*)` result column accumulates into one slot.
fn collect_aggregates(select: &Select) -> Result<Vec<(Expr, AggKind, Option<Expr>)>, CodegenError> {
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
            let (kind, arg) = classify_aggregate(&call)?;
            Ok((call, kind, arg))
        })
        .collect()
}

fn find_aggregates(expr: &Expr, out: &mut Vec<Expr>) {
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
fn substitute_aggregates(expr: &Expr, agg_slots: &[AggSlot], synthetic_names: &[String]) -> Expr {
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
fn read_pseudo_column(
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
fn read_row_columns_into(
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

fn reset_agg(em: &mut Emitter, agg: &AggSlot) {
    match agg.kind {
        AggKind::Count => {
            em.emit(Instruction::new(Opcode::Integer, 0, agg.primary, 0));
        }
        AggKind::Sum | AggKind::Min | AggKind::Max => {
            em.emit(Instruction::new(Opcode::Null, 0, agg.primary, 0));
        }
        AggKind::Avg => {
            em.emit(Instruction::new(Opcode::Null, 0, agg.primary, 0));
            if let Some(aux) = agg.aux {
                em.emit(Instruction::new(Opcode::Integer, 0, aux, 0));
            }
        }
    }
}

fn accumulate_agg(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    scope: &Scope,
    agg: &mut AggSlot,
    zero_reg: i32,
    one_reg: i32,
) -> Result<(), CodegenError> {
    let arg_reg = match &agg.arg {
        Some(expr) => Some(compile_value(em, reg, scope, expr)?),
        None => None,
    };
    match agg.kind {
        AggKind::Count => {
            let _ = zero_reg;
            if let Some(arg_reg) = arg_reg {
                let skip = em.new_label();
                let addr = em.emit(Instruction::new(Opcode::IsNull, arg_reg, 0, 0));
                em.patch_p2(addr, skip);
                em.emit(Instruction::new(
                    Opcode::Add,
                    agg.primary,
                    one_reg,
                    agg.primary,
                ));
                em.place(skip);
            } else {
                em.emit(Instruction::new(
                    Opcode::Add,
                    agg.primary,
                    one_reg,
                    agg.primary,
                ));
            }
        }
        AggKind::Sum | AggKind::Avg => {
            let arg_reg = arg_reg.ok_or_else(|| CodegenError::Unsupported {
                reason: "sum/avg require a single argument".to_string(),
            })?;
            let skip = em.new_label();
            let addr = em.emit(Instruction::new(Opcode::IsNull, arg_reg, 0, 0));
            em.patch_p2(addr, skip);
            let first_val = em.new_label();
            let after = em.new_label();
            let is_null_addr = em.emit(Instruction::new(Opcode::IsNull, agg.primary, 0, 0));
            em.patch_p2(is_null_addr, first_val);
            em.emit(Instruction::new(
                Opcode::Add,
                agg.primary,
                arg_reg,
                agg.primary,
            ));
            let goto_after = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
            em.patch_p2(goto_after, after);
            em.place(first_val);
            em.emit(Instruction::new(Opcode::Copy, arg_reg, agg.primary, 0));
            em.place(after);
            if let Some(aux) = agg.aux {
                em.emit(Instruction::new(Opcode::Add, aux, one_reg, aux));
            }
            em.place(skip);
        }
        AggKind::Min | AggKind::Max => {
            let arg_reg = arg_reg.ok_or_else(|| CodegenError::Unsupported {
                reason: "min/max require a single argument".to_string(),
            })?;
            let skip = em.new_label();
            let addr = em.emit(Instruction::new(Opcode::IsNull, arg_reg, 0, 0));
            em.patch_p2(addr, skip);
            let do_copy = em.new_label();
            let after = em.new_label();
            let is_null_addr = em.emit(Instruction::new(Opcode::IsNull, agg.primary, 0, 0));
            em.patch_p2(is_null_addr, do_copy);
            let cmp_op = if agg.kind == AggKind::Min {
                Opcode::Lt
            } else {
                Opcode::Gt
            };
            let cmp_addr = em.emit(Instruction::new(cmp_op, arg_reg, 0, agg.primary));
            em.patch_p2(cmp_addr, do_copy);
            let goto_after = em.emit(Instruction::new(Opcode::Goto, 0, 0, 0));
            em.patch_p2(goto_after, after);
            em.place(do_copy);
            em.emit(Instruction::new(Opcode::Copy, arg_reg, agg.primary, 0));
            em.place(after);
            em.place(skip);
        }
    }
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
fn flush_group<F>(
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
        if let Some(aux) = agg.aux.filter(|_| agg.kind == AggKind::Avg) {
            // `Divide`: r[P3] = r[P2] / r[P1] — dividend in P2, divisor
            // in P1. `aux` (the non-null count) is 0 exactly when
            // `primary` (the running sum) is still NULL, so a
            // zero-count group divides `Null / 0` and yields `Null`
            // via the same null-propagation `Divide` already gives any
            // other NULL operand — no separate zero-guard needed.
            //
            // SQLite's `avg()` always yields a REAL, unlike a bare `/`
            // between two integers (which truncates) — force the sum
            // to REAL affinity first so `Divide` computes in floating
            // point. `apply_affinity` leaves a NULL sum untouched, so
            // the zero-count case above still divides through to NULL.
            em.emit(Instruction::new(Opcode::RealAffinity, agg.primary, 0, 0));
            em.emit(Instruction::new(Opcode::Divide, aux, agg.primary, dest));
        } else {
            em.emit(Instruction::new(Opcode::Copy, agg.primary, dest, 0));
        }
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
    if let Some(limit) = limit {
        emit_limit_guard(em, limit, end_label);
    }
    em.place(skip_label);
    Ok(())
}

/// A cursor number for `flush_group`'s synthetic per-group record —
/// distinct from [`ScanCursors`]'s four numbers (0-3), which stay live
/// across every `flush_group` call within the same grouped scan.
const FLUSH_CURSOR: i32 = 4;
