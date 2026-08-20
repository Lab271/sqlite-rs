//! Non-correlated subquery-expression codegen (#238): scalar subqueries
//! (`(SELECT ...)`), `IN (SELECT ...)`/`NOT IN (SELECT ...)`, and
//! `EXISTS (SELECT ...)`/`NOT EXISTS (SELECT ...)`. Materialization
//! only (no coroutines) — each subquery occurrence opens its own table
//! cursor (and, for `IN`, an ephemeral index to hold the materialized
//! result column) via [`RegAlloc::alloc_cursor`], compiles the inner
//! `SELECT`'s own single-table scan inline into the enclosing
//! instruction stream, and either captures its first row's leading
//! column (scalar subquery) or tests row existence (`EXISTS`) or row
//! membership (`IN`).
//!
//! Deliberately out of scope for this pass (see the doc comments on
//! each `compile_*` function below for the exact rejection): correlated
//! subqueries (a column reference inside the subquery that resolves
//! only against the *enclosing* query's scope), subqueries in `FROM`,
//! `ANY`/`ALL`/`SOME`, multi-column `IN`, and a subquery whose own
//! `FROM` has a `JOIN`.

use crate::codegen::expr::{compile_cond, compile_value};
use crate::codegen::select::CodegenError;
use crate::codegen::{CondTargets, Emitter, NullTarget, RegAlloc, Scope, Target};
use crate::parser::ast::{Expr, ExprKind, ResultColumn, Select};
use crate::schema::TableSchema;
use crate::vdbe::{Instruction, Opcode, P4};

/// Resolves a subquery's own single-table `FROM` against `catalog`,
/// rejecting anything this MVP pass doesn't materialize: no `FROM` at
/// all is only valid when the subquery has no column references (e.g.
/// `SELECT (SELECT 1)`), and a `JOIN`ed `FROM` isn't supported.
fn resolve_subquery_schema<'q>(
    subselect: &'q Select,
    catalog: &[TableSchema],
) -> Result<Option<(&'q crate::parser::ast::TableRef, TableSchema)>, CodegenError> {
    let Some(from) = &subselect.from else {
        return Ok(None);
    };
    if !from.joins.is_empty() {
        return Err(CodegenError::Unsupported {
            reason: "a subquery whose own FROM clause has a JOIN is not yet supported"
                .to_string(),
        });
    }
    let schema = catalog
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(&from.first.name))
        .cloned()
        .ok_or_else(|| CodegenError::Unsupported {
            reason: format!(
                "subquery references table {:?}, which isn't visible to this compiler's catalog",
                from.first.name
            ),
        })?;
    Ok(Some((&from.first, schema)))
}

/// A subquery's single projected result-column expression — scalar
/// subqueries and `IN (SELECT ...)` both need exactly one (`SELECT *`/
/// `table.*`/more than one column is `Unsupported`: multi-column `IN` is
/// explicitly out of scope, and a scalar subquery's shape is only
/// defined for one column).
fn single_result_expr(subselect: &Select) -> Result<&Expr, CodegenError> {
    match subselect.columns.as_slice() {
        [ResultColumn::Expr { expr, .. }] => Ok(expr),
        _ => Err(CodegenError::Unsupported {
            reason: "a scalar/IN subquery must project exactly one expression column".to_string(),
        }),
    }
}

/// Correlation detection: walks `expr` for every column reference,
/// resolving each against `inner_scope` (the subquery's own `FROM`
/// table(s), possibly none) first. A reference that fails there but
/// resolves against `outer_scope` is a correlated reference — rejected
/// per this pass's bounded scope, rather than silently mis-compiled (it
/// would otherwise read whatever the *enclosing* query's cursor happens
/// to be positioned on, which is actually well-defined register-wise
/// for expressions but wrong for a non-correlated subquery's compiled
/// shape below, which relies on running the subquery's scan exactly
/// once regardless of the outer row). A reference that resolves in
/// neither scope is a genuine unknown column, reported as such.
///
/// Does not recurse into a nested `Subquery`/`Exists`/`InSubquery`'s
/// own subquery body — that nested subquery gets its own correlation
/// check (against `inner_scope` as its immediate enclosing scope) when
/// it is itself compiled.
fn check_correlation(
    expr: &Expr,
    inner_scope: &Scope,
    outer_scope: &Scope,
) -> Result<(), CodegenError> {
    match &expr.kind {
        ExprKind::Column { table, name, .. } => {
            if inner_scope.resolve(table.as_deref(), name).is_ok() {
                return Ok(());
            }
            if outer_scope.resolve(table.as_deref(), name).is_ok() {
                return Err(CodegenError::Unsupported {
                    reason: "correlated subqueries are not yet supported".to_string(),
                });
            }
            // Neither scope resolved it: surface the honest "no such
            // column" answer rather than the generic correlation
            // message.
            inner_scope.resolve(table.as_deref(), name).map(|_| ())
        }
        ExprKind::Unary { expr: inner, .. }
        | ExprKind::IsNull { expr: inner, .. }
        | ExprKind::Cast { expr: inner, .. }
        | ExprKind::Collate { expr: inner, .. }
        | ExprKind::Paren(inner) => check_correlation(inner, inner_scope, outer_scope),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Is { lhs, rhs, .. } => {
            check_correlation(lhs, inner_scope, outer_scope)?;
            check_correlation(rhs, inner_scope, outer_scope)
        }
        ExprKind::Between { expr, lo, hi, .. } => {
            check_correlation(expr, inner_scope, outer_scope)?;
            check_correlation(lo, inner_scope, outer_scope)?;
            check_correlation(hi, inner_scope, outer_scope)
        }
        ExprKind::In { expr, list, .. } => {
            check_correlation(expr, inner_scope, outer_scope)?;
            for item in list {
                check_correlation(item, inner_scope, outer_scope)?;
            }
            Ok(())
        }
        ExprKind::Like {
            expr,
            pattern,
            escape,
            ..
        } => {
            check_correlation(expr, inner_scope, outer_scope)?;
            check_correlation(pattern, inner_scope, outer_scope)?;
            if let Some(escape) = escape {
                check_correlation(escape, inner_scope, outer_scope)?;
            }
            Ok(())
        }
        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            if let Some(operand) = operand {
                check_correlation(operand, inner_scope, outer_scope)?;
            }
            for (when, then) in whens {
                check_correlation(when, inner_scope, outer_scope)?;
                check_correlation(then, inner_scope, outer_scope)?;
            }
            if let Some(else_) = else_ {
                check_correlation(else_, inner_scope, outer_scope)?;
            }
            Ok(())
        }
        ExprKind::FunctionCall { args, .. } => {
            if let crate::parser::ast::FunctionArgs::List(list) = args {
                for arg in list {
                    check_correlation(arg, inner_scope, outer_scope)?;
                }
            }
            Ok(())
        }
        // The LHS of an `IN (SELECT ...)` is the only field of a
        // nested subquery expression this walk descends into; the
        // subquery bodies themselves are checked independently when
        // compiled.
        ExprKind::InSubquery { expr, .. } => check_correlation(expr, inner_scope, outer_scope),
        ExprKind::Literal(_) | ExprKind::Param(_) | ExprKind::Subquery(_) | ExprKind::Exists { .. } => {
            Ok(())
        }
    }
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
    let Some((table_ref, schema)) = resolved else {
        // No FROM: a single computed expression, evaluated exactly
        // once (no rows to iterate).
        if subselect.where_clause.is_some() {
            return Err(CodegenError::Unsupported {
                reason: "a FROM-less scalar subquery cannot have a WHERE clause".to_string(),
            });
        }
        let col_expr = single_result_expr(subselect)?;
        let empty_scope = Scope::default().with_catalog(catalog);
        check_correlation(col_expr, &empty_scope, outer_scope)?;
        let v = compile_value(em, reg, &empty_scope, col_expr)?;
        em.emit(Instruction::new(Opcode::Copy, v, dest, 0));
        return Ok(dest);
    };
    let _ = table_ref;

    let col_expr = single_result_expr(subselect)?;
    let sub_cursor = reg.alloc_cursor();
    let sub_scope = Scope::single(&schema, sub_cursor).with_catalog(catalog);

    if let Some(where_expr) = &subselect.where_clause {
        check_correlation(where_expr, &sub_scope, outer_scope)?;
    }
    check_correlation(col_expr, &sub_scope, outer_scope)?;

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
    let Some((_, schema)) = resolved else {
        return Err(CodegenError::Unsupported {
            reason: "EXISTS (SELECT ...) requires a FROM clause".to_string(),
        });
    };
    let sub_cursor = reg.alloc_cursor();
    let sub_scope = Scope::single(&schema, sub_cursor).with_catalog(catalog);
    if let Some(where_expr) = &subselect.where_clause {
        check_correlation(where_expr, &sub_scope, outer_scope)?;
    }

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
pub(crate) fn compile_in_subquery(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    outer_scope: &Scope,
    lhs: &Expr,
    subselect: &Select,
    negated: bool,
    targets: CondTargets,
) -> Result<(), CodegenError> {
    let catalog = outer_scope.catalog.clone();
    let resolved = resolve_subquery_schema(subselect, &catalog)?;
    let Some((_, schema)) = resolved else {
        return Err(CodegenError::Unsupported {
            reason: "IN (SELECT ...) requires a FROM clause".to_string(),
        });
    };
    let col_expr = single_result_expr(subselect)?;
    let sub_cursor = reg.alloc_cursor();
    let sub_scope = Scope::single(&schema, sub_cursor).with_catalog(catalog);
    if let Some(where_expr) = &subselect.where_clause {
        check_correlation(where_expr, &sub_scope, outer_scope)?;
    }
    check_correlation(col_expr, &sub_scope, outer_scope)?;

    let l = compile_value(em, reg, outer_scope, lhs)?;

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
