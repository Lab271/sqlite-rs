//! Scalar/`EXISTS`/`IN` subquery-expression compilation — see
//! `super`'s module doc.

use super::from_clause::resolve_subquery_schema;
use super::{select_id, HoistedSubquery};
use crate::codegen::expr::{compile_cond, compile_value};
use crate::codegen::select::{
    compile_grouped_scan, select_has_aggregate, CodegenError, ScanCursors,
};
use crate::codegen::{CondTargets, Emitter, NullTarget, RegAlloc, Scope, Target};
use crate::parser::ast::{Expr, ResultColumn, Select};
use crate::vdbe::{Instruction, Opcode, P4};

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
/// `correlation::hoist_uncorrelated_where_subqueries`), its ephemeral
/// index is already built; reuse the cached cursor instead of
/// delegating to `compile_in_subquery_multi`'s normal per-occurrence
/// materialization.
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
/// `correlation::try_hoist_conjunct` to materialize a hoisted,
/// uncorrelated `IN`-subquery exactly once, before the enclosing scan's
/// `Rewind` (#306), instead of [`compile_in_subquery_multi`]'s normal
/// per-occurrence materialization.
pub(super) fn materialize_in_subquery_index(
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
