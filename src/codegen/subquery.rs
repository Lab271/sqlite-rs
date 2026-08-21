//! Subquery-expression codegen (#238, plus the correlated-subquery
//! follow-up): scalar subqueries (`(SELECT ...)`), `IN (SELECT ...)`/
//! `NOT IN (SELECT ...)`, and `EXISTS (SELECT ...)`/`NOT EXISTS
//! (SELECT ...)`. Materialization only (no coroutines) — each subquery
//! occurrence opens its own table cursor (and, for `IN`, an ephemeral
//! index to hold the materialized result column) via
//! [`RegAlloc::alloc_cursor`], compiles the inner `SELECT`'s own
//! single-table scan inline into the enclosing instruction stream, and
//! either captures its first row's leading column (scalar subquery) or
//! tests row existence (`EXISTS`) or row membership (`IN`).
//!
//! Correlation (a column reference inside the subquery that resolves
//! against the *enclosing* query's scope rather than the subquery's
//! own) works for free under materialization: the subquery's own
//! `Scope` is built with [`Scope::with_outer`] pointing at the
//! enclosing scope, so [`Scope::resolve`] falls back there for any
//! reference the subquery's own tables don't resolve. Because this
//! whole `compile_*` call is inlined at the exact point the subquery
//! expression is evaluated (once per outer row, for a subquery inside
//! a `WHERE`/result-column expression), the outer table's cursor is
//! already correctly positioned on the current row every time this
//! code runs — no coroutine or per-row re-invocation machinery needed.
//!
//! Deliberately out of scope for this pass (see the doc comments on
//! each `compile_*` function below for the exact rejection): `ANY`/
//! `ALL`/`SOME`, and a scalar/`IN`/`EXISTS` subquery-*expression* whose
//! own `FROM` has a `JOIN` (unlike a `FROM`-*subquery*'s own `FROM`
//! having a JOIN, which [`materialize_from_subquery`] (#257) does
//! support). Multi-column `IN` (`(a, b) IN (SELECT ...)`) landed in #251
//! as [`compile_in_subquery_multi`] — it reuses the same ephemeral-index
//! machinery as [`compile_in_subquery`], generalized from a
//! single-register key to a contiguous register range (`Found`/
//! `IdxInsert`'s `P4::Int` key-column-count already supported N > 1).

use crate::codegen::expr::{compile_cond, compile_value};
use crate::codegen::select::{
    compile_grouped_scan, compile_select_joined_scan, compile_select_scan, select_has_aggregate,
    CodegenError, ScanCursors,
};
use crate::codegen::{CondTargets, Emitter, NullTarget, RegAlloc, Scope, Target};
use crate::parser::ast::{BinaryOp, Expr, ExprKind, FunctionArgs, ResultColumn, Select, TableRef};
use crate::schema::TableSchema;
use crate::vdbe::{Instruction, Opcode, P4};
use std::collections::HashMap;

/// Identifies a subquery's own `Select` AST node by pointer identity —
/// stable for the lifetime of a single compile pass, since no codegen
/// step clones a `Select`/`Expr` tree once parsing has produced it. Used
/// to key [`Scope::hoisted`] (#306): the same `Select` reference reached
/// once (to hoist/materialize it before the enclosing scan's `Rewind`)
/// and later, per outer row (from `compile_cond`/`compile_value`'s
/// `InSubquery`/`Subquery` dispatch), must resolve to the same map key.
pub(crate) fn select_id(select: &Select) -> usize {
    std::ptr::from_ref(select) as usize
}

/// What a hoisted (materialized-once-before-the-scan) WHERE-clause
/// subquery (#306) precomputed, stashed in [`Scope::hoisted`]: a scalar
/// subquery's already-populated result register, or an uncorrelated
/// `IN`-subquery's already-built ephemeral membership index's cursor.
#[derive(Debug, Clone, Copy)]
pub(crate) enum HoistedSubquery {
    Scalar { reg: i32 },
    In { eph_cursor: i32 },
}

/// A correlated scalar subquery memoized against a single outer column
/// (#314): `cache_cursor` is a table-mode `OpenEphemeral` cursor holding
/// one `(probe_value, result)` row per distinct value of `probe_column`
/// seen so far, opened once before the enclosing scan's `Rewind`. See
/// [`memoize_correlated_where_subqueries`].
#[derive(Debug, Clone)]
pub(crate) struct MemoizedSubquery {
    pub(crate) cache_cursor: i32,
    pub(crate) probe_column: String,
}

/// Caps how many distinct probe values #314's memoization cache holds
/// before it stops growing (see [`compile_memoized_scalar_subquery`]).
/// The cache is scanned linearly per outer row, so its cost is
/// `O(cap)` per row regardless of the outer table's size — chosen
/// small enough that even the largest tier-1 bench fixture (830k rows,
/// `tests/performance/engine.rs`) stays comfortably under the VDBE's
/// 50M-step guard rail even on a cache-cardinality-heavy query
/// (`830_000 * MAX_MEMO_CACHE_ENTRIES * ~4 instructions/entry`, with
/// margin for the rest of the query's own per-row cost). A
/// low-cardinality correlated column (this cache's actual target —
/// bucket/category/FK) fits comfortably under this cap; a
/// higher-cardinality one just falls back to always-recomputing once
/// the cap is hit, same as never caching at all.
const MAX_MEMO_CACHE_ENTRIES: i32 = 8;

/// Resolves a subquery's own single-table `FROM` against `catalog`,
/// rejecting anything this MVP pass doesn't materialize: no `FROM` at
/// all is only valid when the subquery has no column references (e.g.
/// `SELECT (SELECT 1)`), and a `JOIN`ed `FROM` isn't supported.
pub(crate) fn resolve_subquery_schema(
    subselect: &Select,
    catalog: &[TableSchema],
) -> Result<Option<TableSchema>, CodegenError> {
    let Some(from) = &subselect.from else {
        return Ok(None);
    };
    if !from.joins.is_empty() {
        return Err(CodegenError::Unsupported {
            reason: "a subquery whose own FROM clause has a JOIN is not yet supported".to_string(),
        });
    }
    let Some(name) = from.first.name() else {
        return Err(CodegenError::Unsupported {
            reason: "a subquery-expression's own FROM being itself a subquery is not yet \
                     supported"
                .to_string(),
        });
    };
    let schema = catalog
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .cloned()
        .ok_or_else(|| CodegenError::Unsupported {
            reason: format!(
                "subquery references table {name:?}, which isn't visible to this compiler's \
                 catalog"
            ),
        })?;
    Ok(Some(schema))
}

/// The column names a `FROM`-subquery's own `SELECT` list exposes to the
/// enclosing query (#257) — used to build the synthetic [`TableSchema`]
/// a materialized subquery-in-FROM is bound into `Scope` as.
/// `table_refs`/`schemas` are the subquery's own resolved `FROM` tables,
/// same order, for `*`/`table.*` expansion; an unaliased computed
/// expression falls back to a positional `columnN` name (`N` 1-based),
/// same convention SQLite itself uses for an anonymous result column.
fn subquery_output_columns(
    subquery: &Select,
    table_refs: &[&TableRef],
    schemas: &[TableSchema],
) -> Vec<String> {
    let mut out = Vec::new();
    for (i, col) in subquery.columns.iter().enumerate() {
        match col {
            ResultColumn::Star => {
                for schema in schemas {
                    out.extend(schema.columns.iter().cloned());
                }
            }
            ResultColumn::TableStar { table } => {
                if let Some(schema) = table_refs
                    .iter()
                    .position(|t| t.alias.as_deref().or(t.name()).unwrap_or("") == table)
                    .and_then(|idx| schemas.get(idx))
                {
                    out.extend(schema.columns.iter().cloned());
                }
            }
            ResultColumn::Expr { expr, alias } => {
                let name = alias.clone().unwrap_or_else(|| match &expr.kind {
                    ExprKind::Column { name, .. } => name.clone(),
                    _ => format!("column{}", i.saturating_add(1)),
                });
                out.push(name);
            }
        }
    }
    out
}

/// A `FROM`-subquery's own `FROM` table(s) (#257) — the first table plus
/// every join's table, same order. Split from [`resolve_subquery_schemas`]
/// (rather than returning both together) because a function borrowing
/// from two different reference parameters (`subquery` here, `catalog`
/// there) can't have its output lifetime elided, and this codebase's
/// `make mvl-limit` gate forbids writing an explicit lifetime to spell
/// it out.
fn subquery_own_table_refs(subquery: &Select) -> Result<Vec<&TableRef>, CodegenError> {
    let Some(from) = &subquery.from else {
        return Err(CodegenError::Unsupported {
            reason: "a subquery in FROM must itself have a FROM clause".to_string(),
        });
    };
    Ok(std::iter::once(&from.first)
        .chain(from.joins.iter().map(|j| &j.table))
        .collect())
}

/// Resolves each of `table_refs` against `catalog` — one schema per
/// table, same order. A subquery nested inside another subquery's
/// `FROM` is not yet supported (this pass materializes one level).
fn resolve_subquery_schemas(
    table_refs: &[&TableRef],
    catalog: &[TableSchema],
) -> Result<Vec<TableSchema>, CodegenError> {
    table_refs
        .iter()
        .map(|table_ref| {
            let Some(name) = table_ref.name() else {
                return Err(CodegenError::Unsupported {
                    reason: "a subquery nested inside another subquery's FROM is not yet \
                             supported"
                        .to_string(),
                });
            };
            catalog
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(name))
                .cloned()
                .ok_or_else(|| CodegenError::Unsupported {
                    reason: format!("no such table: {name}"),
                })
        })
        .collect()
}

/// Builds the synthetic [`TableSchema`] (#257) a materialized
/// subquery-in-FROM is bound into `Scope` as — `name` left empty, since
/// only the caller (which has the `TableRef`) knows the subquery's
/// mandatory alias.
fn subquery_result_schema(
    subquery: &Select,
    table_refs: &[&TableRef],
    schemas: &[TableSchema],
) -> TableSchema {
    let columns = subquery_output_columns(subquery, table_refs, schemas);
    TableSchema {
        name: String::new(),
        root_page: 0,
        columns: columns.clone(),
        without_rowid: false,
        strict: false,
        column_types: vec![String::new(); columns.len()],
        is_virtual: false,
        sql: String::new(),
        indexes: Vec::new(),
    }
}

/// Resolves `table_ref` to the [`TableSchema`] the rest of codegen
/// should treat it as: a real catalog lookup by name, or (#257) the
/// synthetic schema describing a `FROM`-subquery's own projected
/// columns (its `name` is `table_ref`'s alias — mandatory for a
/// subquery, enforced by the parser). Used by callers (the `sqlite-rs`
/// CLI, `INSERT ... SELECT`) that need a `TableSchema` up front, before
/// the codegen pass that actually emits the materialization
/// (`compile_select_with_catalog`/`compile_select_joined` call
/// [`materialize_from_subquery`] themselves once compiling).
pub fn resolve_from_table_schema(
    table_ref: &TableRef,
    catalog: &[TableSchema],
) -> Result<TableSchema, CodegenError> {
    match &table_ref.kind {
        crate::parser::ast::TableRefKind::Name(name) => catalog
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or_else(|| CodegenError::Unsupported {
                reason: format!("no such table: {name}"),
            }),
        crate::parser::ast::TableRefKind::Subquery(subquery) => {
            let table_refs = subquery_own_table_refs(subquery)?;
            let schemas = resolve_subquery_schemas(&table_refs, catalog)?;
            let mut schema = subquery_result_schema(subquery, &table_refs, &schemas);
            schema.name = table_ref.alias.clone().unwrap_or_default();
            Ok(schema)
        }
    }
}

/// Materializes a `FROM`-subquery (#257) into an in-memory ephemeral
/// table opened on `dest_cursor`, so the enclosing query can then scan
/// it exactly like a real table cursor (`Rewind`/`Next`/`Column`/
/// `Rowid`). Drives the subquery's own scan through
/// [`compile_select_scan`] (single-table) or [`compile_select_joined_scan`]
/// (its own `FROM` has a JOIN — criterion 3), substituting a row sink
/// that `MakeRecord`s each projected row and `Insert`s it into
/// `dest_cursor` with a freshly `Sequence`d rowid, in place of
/// `ResultRow` — the same substitution #208's `INSERT ... SELECT`
/// codegen uses. Returns the synthetic [`TableSchema`] (`name` left
/// empty — the caller fills in the subquery's alias) describing the
/// materialized table's columns, for the caller to bind into `Scope`.
/// A subquery nested inside another subquery's `FROM` is not yet
/// supported (this pass materializes one level).
pub(crate) fn materialize_from_subquery(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    subquery: &Select,
    catalog: &[TableSchema],
    dest_cursor: i32,
) -> Result<TableSchema, CodegenError> {
    let table_refs = subquery_own_table_refs(subquery)?;
    let schemas = resolve_subquery_schemas(&table_refs, catalog)?;
    let Some(from) = &subquery.from else {
        return Err(CodegenError::Unsupported {
            reason: "a subquery in FROM must itself have a FROM clause".to_string(),
        });
    };
    let synthetic_schema = subquery_result_schema(subquery, &table_refs, &schemas);

    em.emit(Instruction {
        opcode: Opcode::OpenEphemeral,
        p1: dest_cursor,
        p2: 0,
        p3: 0,
        p4: P4::None,
        p5: 1,
    });

    let end_label = em.new_label();
    let mut sink = |em: &mut Emitter, reg: &mut RegAlloc, first: i32, count: i32| {
        let rowid_reg = reg.alloc();
        em.emit(Instruction::new(
            Opcode::Sequence,
            dest_cursor,
            rowid_reg,
            0,
        ));
        let record_reg = reg.alloc();
        em.emit(Instruction::new(
            Opcode::MakeRecord,
            first,
            count,
            record_reg,
        ));
        em.emit(Instruction::new(
            Opcode::Insert,
            dest_cursor,
            rowid_reg,
            record_reg,
        ));
        Ok(())
    };

    if from.joins.is_empty() {
        let schema = schemas.first().ok_or_else(|| CodegenError::Unsupported {
            reason: "materialized subquery FROM has no schema".to_string(),
        })?;
        let cursors = ScanCursors {
            table: reg.alloc_cursor(),
            sort: reg.alloc_cursor(),
            pseudo: reg.alloc_cursor(),
            distinct: reg.alloc_cursor(),
        };
        em.emit(Instruction::new(
            Opcode::OpenRead,
            cursors.table,
            i32::try_from(schema.root_page).unwrap_or(0),
            0,
        ));
        compile_select_scan(
            em, reg, subquery, schema, cursors, end_label, catalog, &mut sink,
        )?;
    } else {
        let cursor_base = reg.alloc_cursor();
        // Reserve `table_count + 2` contiguous cursor numbers (one per
        // joined table, plus the sort/pseudo or distinct cursor
        // `compile_select_joined_scan` may itself derive by offsetting
        // from `cursor_base`) so a later `reg.alloc_cursor()` call (e.g.
        // for a correlated subquery expression inside this subquery)
        // can't collide with a number that function computes by
        // arithmetic rather than by calling `alloc_cursor` itself.
        for _ in 0..schemas.len().saturating_add(1) {
            reg.alloc_cursor();
        }
        compile_select_joined_scan(
            em,
            reg,
            subquery,
            &schemas,
            catalog,
            cursor_base,
            end_label,
            &mut sink,
        )?;
    }
    em.place(end_label);

    Ok(synthetic_schema)
}

/// A subquery's single projected result-column expression — scalar
/// subqueries and single-column `IN (SELECT ...)` both need exactly one
/// (`SELECT *`/`table.*`/more than one column is `Unsupported`); see
/// [`multi_result_exprs`] for the multi-column `IN` counterpart.
fn single_result_expr(subselect: &Select) -> Result<&Expr, CodegenError> {
    match subselect.columns.as_slice() {
        [ResultColumn::Expr { expr, .. }] => Ok(expr),
        _ => Err(CodegenError::Unsupported {
            reason: "a scalar/IN subquery must project exactly one expression column".to_string(),
        }),
    }
}

/// A subquery's N projected result-column expressions for
/// multi-column `IN` (#251) — `SELECT *`/`table.*` isn't supported
/// here (arity must be known statically from the expression list).
fn multi_result_exprs(subselect: &Select) -> Result<Vec<&Expr>, CodegenError> {
    subselect
        .columns
        .iter()
        .map(|c| match c {
            ResultColumn::Expr { expr, .. } => Ok(expr),
            _ => Err(CodegenError::Unsupported {
                reason: "a multi-column IN subquery's result columns must be plain expressions \
                         (no * / table.*)"
                    .to_string(),
            }),
        })
        .collect()
}

/// Compiles each of `exprs` into a value register, requiring the
/// results land in a contiguous range (mirrors `select.rs`'s
/// `MakeRecord` contiguity check) — returns `(first register, count)`.
fn compile_contiguous(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    scope: &Scope,
    exprs: impl IntoIterator<Item = impl std::borrow::Borrow<Expr>>,
    what: &str,
) -> Result<(i32, i32), CodegenError> {
    let mut regs = Vec::new();
    for e in exprs {
        regs.push(compile_value(em, reg, scope, e.borrow())?);
    }
    let Some(&first) = regs.first() else {
        return Err(CodegenError::Unsupported {
            reason: format!("{what} must not be empty"),
        });
    };
    for (i, r) in regs.iter().enumerate() {
        let want = first.saturating_add(i32::try_from(i).unwrap_or(i32::MAX));
        if *r != want {
            return Err(CodegenError::Unsupported {
                reason: format!("{what} must land in contiguous registers"),
            });
        }
    }
    Ok((first, i32::try_from(regs.len()).unwrap_or(0)))
}

/// Compiles a scalar subquery `(SELECT ...)` (#238) into a fresh
/// register: NULL if the subquery yields zero rows, otherwise its
/// first result column's value from the *first* row returned (matching
/// SQLite: more than one row silently takes the first rather than
/// erroring).
pub(crate) fn compile_scalar_subquery(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    subselect: &Select,
) -> Result<i32, CodegenError> {
    if !subselect.order_by.is_empty() || subselect.limit.is_some() {
        return Err(CodegenError::Unsupported {
            reason: "ORDER BY/LIMIT in a scalar subquery is not yet supported".to_string(),
        });
    }
    let dest = reg.alloc();
    em.emit(Instruction::new(Opcode::Null, 0, dest, 0));

    let catalog = outer_scope.catalog.clone();
    let resolved = resolve_subquery_schema(subselect, &catalog)?;
    let Some(schema) = resolved else {
        // No FROM: a single computed expression, evaluated exactly
        // once (no rows to iterate).
        if subselect.where_clause.is_some() {
            return Err(CodegenError::Unsupported {
                reason: "a FROM-less scalar subquery cannot have a WHERE clause".to_string(),
            });
        }
        let col_expr = single_result_expr(subselect)?;
        let empty_scope = Scope::default()
            .with_catalog(catalog)
            .with_outer(outer_scope.clone());
        let v = compile_value(em, reg, &empty_scope, col_expr)?;
        em.emit(Instruction::new(Opcode::Copy, v, dest, 0));
        return Ok(dest);
    };

    let sub_cursor = reg.alloc_cursor();

    em.emit(Instruction::new(
        Opcode::OpenRead,
        sub_cursor,
        i32::try_from(schema.root_page).unwrap_or(0),
        0,
    ));

    if select_has_aggregate(subselect) {
        // #304: the subquery's projected expression contains an
        // aggregate call (e.g. `(SELECT max(x) FROM t ...)`) — route
        // through the same implicit-whole-table-group machinery #287
        // built for a top-level `GROUP BY`-less aggregate query, via
        // its `sink` callback, instead of `compile_value`'s plain
        // (aggregate-rejecting) expression path. `compile_grouped_scan`
        // always emits exactly one finalized group's registers, so the
        // sink just copies the first of them into `dest` — no loop/
        // `Rewind`/`Next`/`WHERE`-skip bookkeeping needed here, that's
        // all internal to `compile_grouped_scan` now.
        let cursors = ScanCursors {
            table: sub_cursor,
            sort: reg.alloc_cursor(),
            pseudo: reg.alloc_cursor(),
            distinct: reg.alloc_cursor(),
        };
        let end_label = em.new_label();
        let mut sink = |em: &mut Emitter, _reg: &mut RegAlloc, first: i32, _count: i32| {
            em.emit(Instruction::new(Opcode::Copy, first, dest, 0));
            Ok(())
        };
        compile_grouped_scan(
            em,
            reg,
            subselect,
            &schema,
            cursors,
            end_label,
            &catalog,
            true,
            Some(outer_scope),
            &mut sink,
        )?;
        em.place(end_label);
        return Ok(dest);
    }

    let col_expr = single_result_expr(subselect)?;
    let sub_scope = Scope::single(&schema, sub_cursor)
        .with_catalog(catalog)
        .with_outer(outer_scope.clone());

    let end_label = em.new_label();
    let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, sub_cursor, 0, 0));
    em.patch_p2(rewind_addr, end_label);
    let loop_start = em.new_label();
    em.place(loop_start);

    let skip = em.new_label();
    if let Some(where_expr) = &subselect.where_clause {
        compile_cond(
            em,
            reg,
            &sub_scope,
            where_expr,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(skip)),
        )?;
    }
    let v = compile_value(em, reg, &sub_scope, col_expr)?;
    em.emit(Instruction::new(Opcode::Copy, v, dest, 0));
    em.goto(end_label);

    em.place(skip);
    let next_addr = em.emit(Instruction::new(Opcode::Next, sub_cursor, 0, 0));
    em.patch_p2(next_addr, loop_start);
    em.place(end_label);
    Ok(dest)
}

/// Compiles `EXISTS (SELECT ...)`/`NOT EXISTS (SELECT ...)` (#238) as a
/// jump: runs the subquery's scan and jumps to the true continuation as
/// soon as one row satisfies its `WHERE` clause (or immediately, if it
/// has none), without materializing anything — cheaper than the
/// scalar/`IN` forms since `EXISTS` never needs a row's actual values.
/// `EXISTS` is always definitely true or false (never SQL's unknown),
/// so `targets.on_null` is not consulted.
pub(crate) fn compile_exists(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    subselect: &Select,
    negated: bool,
    targets: CondTargets,
) -> Result<(), CodegenError> {
    let catalog = outer_scope.catalog.clone();
    let resolved = resolve_subquery_schema(subselect, &catalog)?;
    let Some(schema) = resolved else {
        return Err(CodegenError::Unsupported {
            reason: "EXISTS (SELECT ...) requires a FROM clause".to_string(),
        });
    };
    let sub_cursor = reg.alloc_cursor();
    let sub_scope = Scope::single(&schema, sub_cursor)
        .with_catalog(catalog)
        .with_outer(outer_scope.clone());

    let (exists_true, exists_false) = if negated {
        (targets.on_false, targets.on_true)
    } else {
        (targets.on_true, targets.on_false)
    };
    let (t_label, t_is_new) = crate::codegen::expr::ensure_label(em, exists_true);

    em.emit(Instruction::new(
        Opcode::OpenRead,
        sub_cursor,
        i32::try_from(schema.root_page).unwrap_or(0),
        0,
    ));
    let not_found = em.new_label();
    let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, sub_cursor, 0, 0));
    em.patch_p2(rewind_addr, not_found);
    let loop_start = em.new_label();
    em.place(loop_start);

    let skip = em.new_label();
    if let Some(where_expr) = &subselect.where_clause {
        compile_cond(
            em,
            reg,
            &sub_scope,
            where_expr,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(skip)),
        )?;
    }
    em.goto(t_label);
    em.place(skip);
    let next_addr = em.emit(Instruction::new(Opcode::Next, sub_cursor, 0, 0));
    em.patch_p2(next_addr, loop_start);
    em.place(not_found);

    if let Target::Jump(fl) = exists_false {
        em.goto(fl);
    }
    if t_is_new {
        em.place(t_label);
    }
    Ok(())
}

/// Compiles `expr IN (SELECT ...)`/`expr NOT IN (SELECT ...)` (#238):
/// materializes the subquery's single result column into a fresh
/// ephemeral index (the same `OpenEphemeral`/`IdxInsert`/`Found`
/// machinery `DISTINCT` uses), then tests `expr`'s value for membership.
/// Known simplification: a NULL `expr` always routes to the unknown
/// (`on_null`) continuation, rather than SQLite's more precise rule
/// that `NULL IN (<empty subquery result>)` is definitely false — this
/// matches the literal-list `IN` form's own documented NULL-handling
/// shape in this compiler.
///
/// A strict N=1 case of [`compile_in_subquery_multi`] — this is a thin
/// wrapper over it with a one-element LHS tuple, so both forms share the
/// exact same ephemeral-index/`Found` codegen.
///
/// #306: if this subquery was hoisted (materialized once, before the
/// enclosing scan's `Rewind`, because it's uncorrelated — see
/// `hoist_uncorrelated_where_subqueries`), its ephemeral index is
/// already built; reuse the cached cursor instead of delegating to
/// `compile_in_subquery_multi`'s normal per-occurrence materialization.
pub(crate) fn compile_in_subquery(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    lhs: &Expr,
    subselect: &Select,
    negated: bool,
    targets: CondTargets,
) -> Result<(), CodegenError> {
    let Some(HoistedSubquery::In { eph_cursor }) =
        outer_scope.hoisted.get(&select_id(subselect)).copied()
    else {
        return compile_in_subquery_multi(
            em,
            reg,
            outer_scope,
            std::slice::from_ref(lhs),
            subselect,
            negated,
            targets,
        );
    };

    let l = compile_value(em, reg, outer_scope, lhs)?;

    let (true_label, true_is_new) = crate::codegen::expr::ensure_label(em, targets.on_true);
    let (false_label, false_is_new) = crate::codegen::expr::ensure_label(em, targets.on_false);
    let (found_label, notfound_label) = if negated {
        (false_label, true_label)
    } else {
        (true_label, false_label)
    };
    let null_label = match targets.on_null {
        NullTarget::True => true_label,
        NullTarget::False => false_label,
    };

    let null_addr = em.emit(Instruction::new(Opcode::IsNull, l, 0, 0));
    em.patch_p2(null_addr, null_label);
    let found_addr = em.emit(Instruction::with_p4(
        Opcode::Found,
        eph_cursor,
        0,
        l,
        P4::Int(1),
    ));
    em.patch_p2(found_addr, found_label);
    em.goto(notfound_label);

    if false_is_new {
        em.place(false_label);
    }
    if true_is_new {
        em.place(true_label);
    }
    Ok(())
}

/// Materializes a single-column `IN`-subquery's result column into a
/// fresh ephemeral membership index, returning the cursor. Used by
/// [`try_hoist_conjunct`] to materialize a hoisted, uncorrelated
/// `IN`-subquery exactly once, before the enclosing scan's `Rewind`
/// (#306), instead of [`compile_in_subquery_multi`]'s normal
/// per-occurrence materialization.
fn materialize_in_subquery_index(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    subselect: &Select,
) -> Result<i32, CodegenError> {
    let catalog = outer_scope.catalog.clone();
    let resolved = resolve_subquery_schema(subselect, &catalog)?;
    let Some(schema) = resolved else {
        return Err(CodegenError::Unsupported {
            reason: "IN (SELECT ...) requires a FROM clause".to_string(),
        });
    };
    let col_expr = single_result_expr(subselect)?;
    let sub_cursor = reg.alloc_cursor();
    let sub_scope = Scope::single(&schema, sub_cursor)
        .with_catalog(catalog)
        .with_outer(outer_scope.clone());

    let eph_cursor = reg.alloc_cursor();
    em.emit(Instruction::new(Opcode::OpenEphemeral, eph_cursor, 0, 0));

    em.emit(Instruction::new(
        Opcode::OpenRead,
        sub_cursor,
        i32::try_from(schema.root_page).unwrap_or(0),
        0,
    ));
    let scan_end = em.new_label();
    let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, sub_cursor, 0, 0));
    em.patch_p2(rewind_addr, scan_end);
    let loop_start = em.new_label();
    em.place(loop_start);

    let skip = em.new_label();
    if let Some(where_expr) = &subselect.where_clause {
        compile_cond(
            em,
            reg,
            &sub_scope,
            where_expr,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(skip)),
        )?;
    }
    let v = compile_value(em, reg, &sub_scope, col_expr)?;
    em.emit(Instruction::with_p4(
        Opcode::IdxInsert,
        eph_cursor,
        v,
        0,
        P4::Int(1),
    ));
    em.place(skip);
    let next_addr = em.emit(Instruction::new(Opcode::Next, sub_cursor, 0, 0));
    em.patch_p2(next_addr, loop_start);
    em.place(scan_end);
    Ok(eph_cursor)
}

/// Whether `subselect` is correlated — references a column that isn't
/// one of its own `own_schema`'s columns (#306). `own_schema: None`
/// (a `FROM`-less subquery) is always treated as correlated: a
/// `FROM`-less subquery can only reference the enclosing scope, and
/// hoisting has nothing to gain there anyway. Walks `subselect`'s own
/// `WHERE` clause and projected result-column expressions; a nested
/// subquery-bearing expression found anywhere in that walk is
/// conservatively treated as correlated too, rather than reasoning
/// through another scope level. Correlated=true is always the *safe*
/// answer when this check is unsure — it only ever suppresses hoisting,
/// never wrongly allows it.
pub(crate) fn subquery_is_correlated(subselect: &Select, own_schema: Option<&TableSchema>) -> bool {
    let Some(schema) = own_schema else {
        return true;
    };
    // A column reference qualified by a table name (`other.a_id`) only
    // counts as "this subquery's own" when the qualifier names the
    // subquery's own table or its alias — matching `schema.name` alone
    // would wrongly call `WHERE other.a_id = t.id` uncorrelated whenever
    // the *outer* table happens to share a column name with the
    // subquery's own table (`t.id`/`other.id` in #306's regression
    // fixture), since a bare name-only check can't tell `t.id` isn't
    // `other`'s own `id`.
    let own_qualifiers: Vec<&str> = std::iter::once(schema.name.as_str())
        .chain(
            subselect
                .from
                .as_ref()
                .and_then(|f| f.first.alias.as_deref()),
        )
        .collect();
    let mut correlated = false;
    if let Some(where_expr) = &subselect.where_clause {
        walk_expr_for_correlation(where_expr, schema, &own_qualifiers, &mut correlated);
    }
    for col in &subselect.columns {
        match col {
            ResultColumn::Expr { expr, .. } => {
                walk_expr_for_correlation(expr, schema, &own_qualifiers, &mut correlated);
            }
            // `*`/`table.*` project only the subquery's own columns —
            // never a reference to the enclosing scope.
            ResultColumn::Star | ResultColumn::TableStar { .. } => {}
        }
        if correlated {
            break;
        }
    }
    correlated
}

fn walk_expr_for_correlation(
    expr: &Expr,
    schema: &TableSchema,
    own_qualifiers: &[&str],
    correlated: &mut bool,
) {
    if *correlated {
        return;
    }
    match &expr.kind {
        ExprKind::Column { table, name, .. } => {
            let qualifier_ok = match table {
                Some(t) => own_qualifiers.iter().any(|q| q.eq_ignore_ascii_case(t)),
                None => true,
            };
            if !qualifier_ok || !schema.columns.iter().any(|c| c.eq_ignore_ascii_case(name)) {
                *correlated = true;
            }
        }
        ExprKind::Literal(_) | ExprKind::Param(_) => {}
        ExprKind::FunctionCall { args, .. } => {
            if let FunctionArgs::List(list) = args {
                for a in list {
                    walk_expr_for_correlation(a, schema, own_qualifiers, correlated);
                }
            }
        }
        ExprKind::Unary { expr: e, .. }
        | ExprKind::IsNull { expr: e, .. }
        | ExprKind::Cast { expr: e, .. }
        | ExprKind::Collate { expr: e, .. }
        | ExprKind::Paren(e) => walk_expr_for_correlation(e, schema, own_qualifiers, correlated),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Is { lhs, rhs, .. } => {
            walk_expr_for_correlation(lhs, schema, own_qualifiers, correlated);
            walk_expr_for_correlation(rhs, schema, own_qualifiers, correlated);
        }
        ExprKind::Between {
            expr: e, lo, hi, ..
        } => {
            walk_expr_for_correlation(e, schema, own_qualifiers, correlated);
            walk_expr_for_correlation(lo, schema, own_qualifiers, correlated);
            walk_expr_for_correlation(hi, schema, own_qualifiers, correlated);
        }
        ExprKind::In { expr: e, list, .. } => {
            walk_expr_for_correlation(e, schema, own_qualifiers, correlated);
            for item in list {
                walk_expr_for_correlation(item, schema, own_qualifiers, correlated);
            }
        }
        ExprKind::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            walk_expr_for_correlation(e, schema, own_qualifiers, correlated);
            walk_expr_for_correlation(pattern, schema, own_qualifiers, correlated);
            if let Some(esc) = escape {
                walk_expr_for_correlation(esc, schema, own_qualifiers, correlated);
            }
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            if let Some(o) = operand {
                walk_expr_for_correlation(o, schema, own_qualifiers, correlated);
            }
            for (w, t) in whens {
                walk_expr_for_correlation(w, schema, own_qualifiers, correlated);
                walk_expr_for_correlation(t, schema, own_qualifiers, correlated);
            }
            if let Some(e) = else_ {
                walk_expr_for_correlation(e, schema, own_qualifiers, correlated);
            }
        }
        // Nested subquery-bearing expressions: conservatively correlated
        // rather than reasoning through another scope level (out of
        // scope for #306's hoist pass).
        ExprKind::Subquery(_)
        | ExprKind::Exists { .. }
        | ExprKind::InSubquery { .. }
        | ExprKind::InSubqueryMulti { .. } => {
            *correlated = true;
        }
    }
}

fn is_comparison_op(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
    )
}

/// Whether `subquery` is a candidate for #306's hoist: it has a
/// (non-joined, single-table) `FROM` this pass can resolve, and it's
/// not correlated against the enclosing query.
fn subquery_hoistable(subquery: &Select, outer_scope: &Scope) -> bool {
    let Ok(resolved) = resolve_subquery_schema(subquery, &outer_scope.catalog) else {
        return false;
    };
    let Some(schema) = resolved else {
        // FROM-less: nothing to gain from hoisting (no per-row scan
        // cost to eliminate), and `subquery_is_correlated` already
        // treats it as correlated anyway.
        return false;
    };
    !subquery_is_correlated(subquery, Some(&schema))
}

/// The top-level `AND`-conjuncts of `expr` — splits only `AND` (and
/// `Paren`-wrapped `AND`), leaving any `OR`/`NOT`/other nesting as a
/// single opaque conjunct. Used by
/// [`hoist_uncorrelated_where_subqueries`] to find a WHERE clause's
/// directly-AND-joined subquery conjuncts without having to reason
/// about deeper nesting.
fn top_level_and_conjuncts(expr: &Expr) -> Vec<&Expr> {
    match &expr.kind {
        ExprKind::Binary {
            op: BinaryOp::And,
            lhs,
            rhs,
        } => {
            let mut out = top_level_and_conjuncts(lhs);
            out.extend(top_level_and_conjuncts(rhs));
            out
        }
        ExprKind::Paren(inner) => top_level_and_conjuncts(inner),
        _ => vec![expr],
    }
}

/// Recognizes and materializes one hoistable conjunct: a top-level
/// `expr IN (SELECT ...)`, or a top-level comparison with a scalar
/// subquery operand — the two shapes #306's reproduction actually hits.
/// Returns the subquery's own [`select_id`] (for the caller to key the
/// returned map) plus what got precomputed, or `None` if `conjunct`
/// doesn't match either shape or its subquery turns out to be
/// correlated (in which case nothing is emitted and the caller's
/// existing per-row path handles it unchanged). Returns the `usize` key
/// rather than the `&Select` itself — `conjunct`/`outer_scope` are two
/// independent borrowed parameters, so the qualified subset's ban on
/// explicit lifetimes has no elidable shape that could tie an `&Select`
/// return to `conjunct`'s borrow alone.
fn try_hoist_conjunct(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    conjunct: &Expr,
) -> Result<Option<(usize, HoistedSubquery)>, CodegenError> {
    match &conjunct.kind {
        ExprKind::InSubquery { subquery, .. } if subquery_hoistable(subquery, outer_scope) => {
            let eph_cursor = materialize_in_subquery_index(em, reg, outer_scope, subquery)?;
            Ok(Some((
                select_id(subquery),
                HoistedSubquery::In { eph_cursor },
            )))
        }
        ExprKind::Binary { op, lhs, rhs } if is_comparison_op(*op) => {
            for side in [lhs.as_ref(), rhs.as_ref()] {
                if let ExprKind::Subquery(subquery) = &side.kind {
                    if subquery_hoistable(subquery, outer_scope) {
                        let dest = compile_scalar_subquery(em, reg, outer_scope, subquery)?;
                        return Ok(Some((
                            select_id(subquery),
                            HoistedSubquery::Scalar { reg: dest },
                        )));
                    }
                }
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// Hoists every uncorrelated `IN`/scalar subquery found as a top-level
/// `WHERE`-clause conjunct (#306) out of the enclosing single-table
/// scan's per-row loop: each is materialized exactly once here, *before*
/// the scan's `Rewind`, instead of being re-materialized on every outer
/// row by `compile_in_subquery`/`compile_scalar_subquery`'s normal
/// per-occurrence codegen. Returns a map (from [`select_id`] to what got
/// precomputed) meant to be attached to the scan's own [`Scope`] via
/// [`Scope::with_hoisted`] — `compile_cond`/`compile_value`'s
/// `InSubquery`/`Subquery` dispatch then read the cheap precomputed
/// cursor/register per row instead of rebuilding it.
///
/// Deliberately narrow, matching the issue's own reproduction: only a
/// conjunct that is *exactly* `expr IN (SELECT ...)` or a top-level
/// comparison with a scalar-subquery operand is recognized (see
/// [`try_hoist_conjunct`]); `OR`, `NOT`, deeper nesting, multi-column
/// `IN`, and any correlated subquery are left completely untouched and
/// fall through to the existing, unmodified per-row path. Also
/// deliberately scoped to the single-table scan path
/// (`compile_direct_scan`/`compile_sorted_scan`) — the joined-query path
/// (`compile_join_level`/`emit_join_final_row`) is a follow-up this pass
/// does not attempt, so a joined query's WHERE-clause subquery keeps
/// re-evaluating per candidate row exactly as before.
pub(crate) fn hoist_uncorrelated_where_subqueries(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    where_clause: &Expr,
) -> Result<HashMap<usize, HoistedSubquery>, CodegenError> {
    let mut out = HashMap::new();
    for conjunct in top_level_and_conjuncts(where_clause) {
        if let Some((key, hoisted)) = try_hoist_conjunct(em, reg, outer_scope, conjunct)? {
            out.insert(key, hoisted);
        }
    }
    Ok(out)
}

/// Walks `expr` collecting the single outer column `subselect` (whose
/// own schema is `own_schema`) is correlated against, mirroring
/// [`walk_expr_for_correlation`]'s traversal shape but gathering an
/// identifier instead of a bare bool. Sets `*ambiguous` (and gives up)
/// on: a reference that resolves to neither `own_schema` nor
/// `outer_schema` (out of this pass's single-table-correlation scope
/// entirely — e.g. a deeper `outer.outer` reference), a *second*
/// distinct outer column (the memoization cache below only has room for
/// one probe value), or any nested subquery-bearing expression
/// (conservatively out of scope, same as the #306 correlation check).
#[allow(clippy::too_many_arguments)]
fn collect_correlated_column(
    expr: &Expr,
    own_schema: &TableSchema,
    own_qualifiers: &[&str],
    outer_schema: &TableSchema,
    found: &mut Option<String>,
    ambiguous: &mut bool,
) {
    if *ambiguous {
        return;
    }
    match &expr.kind {
        ExprKind::Column { table, name, .. } => {
            let qualifier_ok = match table {
                Some(t) => own_qualifiers.iter().any(|q| q.eq_ignore_ascii_case(t)),
                None => true,
            };
            let is_own = qualifier_ok
                && own_schema
                    .columns
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(name));
            if is_own {
                return;
            }
            if !outer_schema
                .columns
                .iter()
                .any(|c| c.eq_ignore_ascii_case(name))
            {
                *ambiguous = true;
                return;
            }
            match found {
                Some(existing) if existing.eq_ignore_ascii_case(name) => {}
                Some(_) => *ambiguous = true,
                None => *found = Some(name.clone()),
            }
        }
        ExprKind::Literal(_) | ExprKind::Param(_) => {}
        ExprKind::FunctionCall { args, .. } => {
            if let FunctionArgs::List(list) = args {
                for a in list {
                    collect_correlated_column(
                        a,
                        own_schema,
                        own_qualifiers,
                        outer_schema,
                        found,
                        ambiguous,
                    );
                }
            }
        }
        ExprKind::Unary { expr: e, .. }
        | ExprKind::IsNull { expr: e, .. }
        | ExprKind::Cast { expr: e, .. }
        | ExprKind::Collate { expr: e, .. }
        | ExprKind::Paren(e) => collect_correlated_column(
            e,
            own_schema,
            own_qualifiers,
            outer_schema,
            found,
            ambiguous,
        ),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Is { lhs, rhs, .. } => {
            collect_correlated_column(
                lhs,
                own_schema,
                own_qualifiers,
                outer_schema,
                found,
                ambiguous,
            );
            collect_correlated_column(
                rhs,
                own_schema,
                own_qualifiers,
                outer_schema,
                found,
                ambiguous,
            );
        }
        ExprKind::Between {
            expr: e, lo, hi, ..
        } => {
            collect_correlated_column(
                e,
                own_schema,
                own_qualifiers,
                outer_schema,
                found,
                ambiguous,
            );
            collect_correlated_column(
                lo,
                own_schema,
                own_qualifiers,
                outer_schema,
                found,
                ambiguous,
            );
            collect_correlated_column(
                hi,
                own_schema,
                own_qualifiers,
                outer_schema,
                found,
                ambiguous,
            );
        }
        ExprKind::In { expr: e, list, .. } => {
            collect_correlated_column(
                e,
                own_schema,
                own_qualifiers,
                outer_schema,
                found,
                ambiguous,
            );
            for item in list {
                collect_correlated_column(
                    item,
                    own_schema,
                    own_qualifiers,
                    outer_schema,
                    found,
                    ambiguous,
                );
            }
        }
        ExprKind::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            collect_correlated_column(
                e,
                own_schema,
                own_qualifiers,
                outer_schema,
                found,
                ambiguous,
            );
            collect_correlated_column(
                pattern,
                own_schema,
                own_qualifiers,
                outer_schema,
                found,
                ambiguous,
            );
            if let Some(esc) = escape {
                collect_correlated_column(
                    esc,
                    own_schema,
                    own_qualifiers,
                    outer_schema,
                    found,
                    ambiguous,
                );
            }
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            if let Some(o) = operand {
                collect_correlated_column(
                    o,
                    own_schema,
                    own_qualifiers,
                    outer_schema,
                    found,
                    ambiguous,
                );
            }
            for (w, t) in whens {
                collect_correlated_column(
                    w,
                    own_schema,
                    own_qualifiers,
                    outer_schema,
                    found,
                    ambiguous,
                );
                collect_correlated_column(
                    t,
                    own_schema,
                    own_qualifiers,
                    outer_schema,
                    found,
                    ambiguous,
                );
            }
            if let Some(e) = else_ {
                collect_correlated_column(
                    e,
                    own_schema,
                    own_qualifiers,
                    outer_schema,
                    found,
                    ambiguous,
                );
            }
        }
        ExprKind::Subquery(_)
        | ExprKind::Exists { .. }
        | ExprKind::InSubquery { .. }
        | ExprKind::InSubqueryMulti { .. } => {
            *ambiguous = true;
        }
    }
}

/// Whether `subselect` (whose own schema is `own_schema`) is correlated
/// against exactly one column of `outer_schema` — the shape #314's
/// memoization cache needs a single probe value for. `None` if zero
/// columns, more than one distinct column, or anything
/// [`collect_correlated_column`] can't reason about.
fn single_correlated_outer_column(
    subselect: &Select,
    own_schema: &TableSchema,
    outer_schema: &TableSchema,
) -> Option<String> {
    let own_qualifiers: Vec<&str> = std::iter::once(own_schema.name.as_str())
        .chain(
            subselect
                .from
                .as_ref()
                .and_then(|f| f.first.alias.as_deref()),
        )
        .collect();
    let mut found = None;
    let mut ambiguous = false;
    if let Some(where_expr) = &subselect.where_clause {
        collect_correlated_column(
            where_expr,
            own_schema,
            &own_qualifiers,
            outer_schema,
            &mut found,
            &mut ambiguous,
        );
    }
    for col in &subselect.columns {
        if let ResultColumn::Expr { expr, .. } = col {
            collect_correlated_column(
                expr,
                own_schema,
                &own_qualifiers,
                outer_schema,
                &mut found,
                &mut ambiguous,
            );
        }
    }
    if ambiguous {
        None
    } else {
        found
    }
}

/// Whether `subquery` is a candidate for #314's memoization cache: it
/// has a (non-joined, single-table) `FROM` this pass can resolve, it
/// *is* correlated (#306's hoist already handles the uncorrelated
/// case), and it's correlated against exactly one column of
/// `outer_schema`.
fn subquery_memoizable(
    subquery: &Select,
    outer_schema: &TableSchema,
    outer_scope: &Scope,
) -> Option<String> {
    let resolved = resolve_subquery_schema(subquery, &outer_scope.catalog).ok()??;
    if !subquery_is_correlated(subquery, Some(&resolved)) {
        return None;
    }
    single_correlated_outer_column(subquery, &resolved, outer_schema)
}

/// Recognizes one memoizable conjunct: a top-level comparison with a
/// correlated (single-outer-column) scalar-subquery operand. Emits the
/// subquery's cache table (`OpenEphemeral`, table mode, empty at this
/// point — populated lazily, per distinct probe value, by
/// [`compile_memoized_scalar_subquery`]) and returns its
/// [`select_id`] plus the [`MemoizedSubquery`] handle, or `None` if
/// `conjunct` doesn't match this shape.
fn try_memoize_conjunct(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    outer_schema: &TableSchema,
    conjunct: &Expr,
) -> Option<(usize, MemoizedSubquery)> {
    let ExprKind::Binary { op, lhs, rhs } = &conjunct.kind else {
        return None;
    };
    if !is_comparison_op(*op) {
        return None;
    }
    for side in [lhs.as_ref(), rhs.as_ref()] {
        if let ExprKind::Subquery(subquery) = &side.kind {
            if let Some(probe_column) = subquery_memoizable(subquery, outer_schema, outer_scope) {
                let cache_cursor = reg.alloc_cursor();
                em.emit(Instruction {
                    opcode: Opcode::OpenEphemeral,
                    p1: cache_cursor,
                    p2: 0,
                    p3: 0,
                    p4: P4::None,
                    p5: 1,
                });
                return Some((
                    select_id(subquery),
                    MemoizedSubquery {
                        cache_cursor,
                        probe_column,
                    },
                ));
            }
        }
    }
    None
}

/// Sets up #314's per-probe-value memoization cache for every
/// correlated, single-outer-column scalar subquery found as a top-level
/// `WHERE`-clause conjunct, mirroring [`hoist_uncorrelated_where_subqueries`]'s
/// structure and scope boundary (single-table `WHERE`-clause scans
/// only; a joined query's `WHERE` clause is not attempted). Returns a
/// map meant to be attached to the scan's own [`Scope`] via
/// [`Scope::with_memoized`] — [`compile_value`]'s `Subquery` dispatch
/// then routes through [`compile_memoized_scalar_subquery`] instead of
/// unconditionally re-running the subquery every row.
///
/// Deliberately scalar-only: an `IN (SELECT ...)` subquery's per-probe-
/// value "result" is a whole membership set, not a single value, which
/// would need a cache of ephemeral indexes rather than a cache of
/// scalars — a larger follow-up left for a future ticket rather than
/// folded into this one's narrower scope.
pub(crate) fn memoize_correlated_where_subqueries(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    outer_schema: &TableSchema,
    where_clause: &Expr,
) -> HashMap<usize, MemoizedSubquery> {
    let mut out = HashMap::new();
    for conjunct in top_level_and_conjuncts(where_clause) {
        if let Some((key, memo)) =
            try_memoize_conjunct(em, reg, outer_scope, outer_schema, conjunct)
        {
            out.insert(key, memo);
        }
    }
    out
}

/// Compiles a memoized correlated scalar subquery (#314): reads the
/// current outer row's `memo.probe_column` value, linearly scans
/// `memo.cache_cursor` (a small table of `(probe_value, result)` rows —
/// one per *distinct* probe value seen so far, not one per outer row)
/// for a match, and on a hit copies the cached result straight out —
/// skipping [`compile_scalar_subquery`]'s whole inner scan entirely. On
/// a miss (including every NULL probe value, which never caches — SQL's
/// `NULL = NULL` is unknown, not true), runs the subquery normally and,
/// for a non-NULL probe, appends the result to the cache for the next
/// outer row with the same value to hit.
///
/// The cache-hit comparison is a plain, uncollated `Eq` — never a false
/// positive (an actual SQL-distinct pair of values is never judged
/// equal by a *stricter* byte-exact comparison), so correctness is
/// preserved even for a `NOCASE`-collated probe column; the only cost
/// is a few avoidable cache misses in that case.
pub(crate) fn compile_memoized_scalar_subquery(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    subselect: &Select,
    memo: &MemoizedSubquery,
) -> Result<i32, CodegenError> {
    let binding = outer_scope
        .tables
        .first()
        .ok_or_else(|| CodegenError::Unsupported {
            reason: "memoized correlated subquery has no outer table binding".to_string(),
        })?;
    let col_idx = crate::codegen::expr::column_index(&binding.schema, &memo.probe_column)
        .ok_or_else(|| CodegenError::Unsupported {
            reason: format!(
                "memoized correlated subquery's probe column {:?} not found on the outer table",
                memo.probe_column
            ),
        })?;

    let dest = reg.alloc();
    let probe_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::Column,
        binding.cursor,
        i32::try_from(col_idx).unwrap_or(0),
        probe_reg,
    ));

    let end_label = em.new_label();
    let null_probe_label = em.new_label();
    let miss_label = em.new_label();
    let hit_label = em.new_label();

    let null_addr = em.emit(Instruction::new(Opcode::IsNull, probe_reg, 0, 0));
    em.patch_p2(null_addr, null_probe_label);

    let cmp_reg = reg.alloc();
    let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, memo.cache_cursor, 0, 0));
    em.patch_p2(rewind_addr, miss_label);
    let loop_start = em.new_label();
    em.place(loop_start);
    em.emit(Instruction::new(
        Opcode::Column,
        memo.cache_cursor,
        0,
        cmp_reg,
    ));
    let eq_addr = em.emit(Instruction::new(Opcode::Eq, cmp_reg, 0, probe_reg));
    em.patch_p2(eq_addr, hit_label);
    let next_addr = em.emit(Instruction::new(Opcode::Next, memo.cache_cursor, 0, 0));
    em.patch_p2(next_addr, loop_start);
    em.goto(miss_label);

    em.place(hit_label);
    em.emit(Instruction::new(Opcode::Column, memo.cache_cursor, 1, dest));
    em.goto(end_label);

    em.place(miss_label);
    let fresh = compile_scalar_subquery(em, reg, outer_scope, subselect)?;
    em.emit(Instruction::new(Opcode::Copy, fresh, dest, 0));
    let rowid_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::Sequence,
        memo.cache_cursor,
        rowid_reg,
        0,
    ));
    // `Sequence` returns this row's 1-based ordinal — once it exceeds
    // MAX_MEMO_CACHE_ENTRIES, stop growing the cache rather than let the
    // per-row linear scan above grow unbounded with it. A high-
    // cardinality correlated column (one a cache can't meaningfully help
    // anyway) then falls back to exactly today's always-recompute
    // behavior for every value past the cap — the "no regression"
    // guarantee — while a low-cardinality one (this cache's actual
    // target: a bucket/category/FK column) still gets fully cached.
    let cap_reg = reg.alloc();
    em.emit(Instruction::new(
        Opcode::Integer,
        MAX_MEMO_CACHE_ENTRIES,
        cap_reg,
        0,
    ));
    let over_cap_addr = em.emit(Instruction::new(Opcode::Gt, rowid_reg, 0, cap_reg));
    em.patch_p2(over_cap_addr, end_label);
    let key_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Copy, probe_reg, key_reg, 0));
    let val_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Copy, dest, val_reg, 0));
    let record_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::MakeRecord, key_reg, 2, record_reg));
    em.emit(Instruction::new(
        Opcode::Insert,
        memo.cache_cursor,
        rowid_reg,
        record_reg,
    ));
    em.goto(end_label);

    em.place(null_probe_label);
    let fresh_null = compile_scalar_subquery(em, reg, outer_scope, subselect)?;
    em.emit(Instruction::new(Opcode::Copy, fresh_null, dest, 0));

    em.place(end_label);
    Ok(dest)
}

/// Compiles `(a, b, ...) IN (SELECT ...)`/`... NOT IN (SELECT ...)`
/// (#251): the multi-column generalization of [`compile_in_subquery`].
/// Materializes the subquery's N projected columns into a fresh
/// ephemeral index keyed on all N (`Found`/`IdxInsert`'s `P4::Int`
/// key-column-count, already N-column-capable — see
/// `vdbe/cursor.rs::found`/`idx_insert`), then tests the LHS tuple's N
/// values for membership the same way. Requires the LHS tuple and the
/// subquery's projection to compile into contiguous register ranges
/// (`compile_contiguous`) and to have matching arity. NULL handling
/// mirrors [`compile_in_subquery`]: any NULL component in the LHS tuple
/// routes to the unknown (`on_null`) continuation.
pub(crate) fn compile_in_subquery_multi(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    lhs_exprs: &[Expr],
    subselect: &Select,
    negated: bool,
    targets: CondTargets,
) -> Result<(), CodegenError> {
    let catalog = outer_scope.catalog.clone();
    let resolved = resolve_subquery_schema(subselect, &catalog)?;
    let Some(schema) = resolved else {
        return Err(CodegenError::Unsupported {
            reason: "IN (SELECT ...) requires a FROM clause".to_string(),
        });
    };
    let col_exprs = multi_result_exprs(subselect)?;
    if col_exprs.len() != lhs_exprs.len() {
        return Err(CodegenError::Unsupported {
            reason: format!(
                "multi-column IN: left-hand tuple has {} column(s) but the subquery projects {}",
                lhs_exprs.len(),
                col_exprs.len()
            ),
        });
    }
    let sub_cursor = reg.alloc_cursor();
    let sub_scope = Scope::single(&schema, sub_cursor)
        .with_catalog(catalog)
        .with_outer(outer_scope.clone());

    let (l_first, l_count) = compile_contiguous(
        em,
        reg,
        outer_scope,
        lhs_exprs.iter(),
        "multi-column IN's left-hand tuple",
    )?;

    let eph_cursor = reg.alloc_cursor();
    em.emit(Instruction::new(Opcode::OpenEphemeral, eph_cursor, 0, 0));

    em.emit(Instruction::new(
        Opcode::OpenRead,
        sub_cursor,
        i32::try_from(schema.root_page).unwrap_or(0),
        0,
    ));
    let scan_end = em.new_label();
    let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, sub_cursor, 0, 0));
    em.patch_p2(rewind_addr, scan_end);
    let loop_start = em.new_label();
    em.place(loop_start);

    let skip = em.new_label();
    if let Some(where_expr) = &subselect.where_clause {
        compile_cond(
            em,
            reg,
            &sub_scope,
            where_expr,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(skip)),
        )?;
    }
    let (v_first, v_count) = compile_contiguous(
        em,
        reg,
        &sub_scope,
        col_exprs.iter().copied(),
        "multi-column IN's subquery projection",
    )?;
    em.emit(Instruction::with_p4(
        Opcode::IdxInsert,
        eph_cursor,
        v_first,
        0,
        P4::Int(v_count.into()),
    ));
    em.place(skip);
    let next_addr = em.emit(Instruction::new(Opcode::Next, sub_cursor, 0, 0));
    em.patch_p2(next_addr, loop_start);
    em.place(scan_end);

    let (true_label, true_is_new) = crate::codegen::expr::ensure_label(em, targets.on_true);
    let (false_label, false_is_new) = crate::codegen::expr::ensure_label(em, targets.on_false);
    let (found_label, notfound_label) = if negated {
        (false_label, true_label)
    } else {
        (true_label, false_label)
    };
    let null_label = match targets.on_null {
        NullTarget::True => true_label,
        NullTarget::False => false_label,
    };

    for i in 0..l_count {
        let r = l_first.saturating_add(i);
        let null_addr = em.emit(Instruction::new(Opcode::IsNull, r, 0, 0));
        em.patch_p2(null_addr, null_label);
    }
    let found_addr = em.emit(Instruction::with_p4(
        Opcode::Found,
        eph_cursor,
        0,
        l_first,
        P4::Int(l_count.into()),
    ));
    em.patch_p2(found_addr, found_label);
    em.goto(notfound_label);

    if false_is_new {
        em.place(false_label);
    }
    if true_is_new {
        em.place(true_label);
    }
    Ok(())
}
