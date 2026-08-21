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
    compile_select_joined_scan, compile_select_scan, CodegenError, ScanCursors,
};
use crate::codegen::{CondTargets, Emitter, NullTarget, RegAlloc, Scope, Target};
use crate::parser::ast::{Expr, ExprKind, ResultColumn, Select, TableRef};
use crate::schema::TableSchema;
use crate::vdbe::{Instruction, Opcode, P4};

/// Resolves a subquery's own single-table `FROM` against `catalog`,
/// rejecting anything this MVP pass doesn't materialize: no `FROM` at
/// all is only valid when the subquery has no column references (e.g.
/// `SELECT (SELECT 1)`), and a `JOIN`ed `FROM` isn't supported.
fn resolve_subquery_schema(
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
            em, reg, subquery, schema, cursors, end_label, &schemas, &mut sink,
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

    let col_expr = single_result_expr(subselect)?;
    let sub_cursor = reg.alloc_cursor();
    let sub_scope = Scope::single(&schema, sub_cursor)
        .with_catalog(catalog)
        .with_outer(outer_scope.clone());

    em.emit(Instruction::new(
        Opcode::OpenRead,
        sub_cursor,
        i32::try_from(schema.root_page).unwrap_or(0),
        0,
    ));
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
pub(crate) fn compile_in_subquery(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    lhs: &Expr,
    subselect: &Select,
    negated: bool,
    targets: CondTargets,
) -> Result<(), CodegenError> {
    compile_in_subquery_multi(
        em,
        reg,
        outer_scope,
        std::slice::from_ref(lhs),
        subselect,
        negated,
        targets,
    )
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
