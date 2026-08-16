//! Expression lowering (spec 009, Requirement 11): boolean-valued
//! expressions compile to jump instructions targeting a true/false
//! continuation, never an intermediate boolean register — the classic
//! jumping-code-generation technique. `compile_cond` is the jump-mode
//! entry point; `compile_value` is the ordinary register-producing
//! entry point used for result columns, function arguments, and CASE
//! branch results.

use crate::codegen::{p4_coll_seq, CodegenError, Emitter, Label, RegAlloc, Target};
use crate::parser::ast::{BinaryOp, Expr, ExprKind, Literal, UnaryOp};
use crate::schema::TableSchema;
use crate::vdbe::{Collation, Instruction, Opcode, P4};

/// Resolves a bare `Expr::Column` name against the schema; any other
/// expression is a codegen error only when a caller specifically
/// requires a plain column (there is no such requirement in this
/// module — kept for callers like `select.rs`'s ORDER BY/DISTINCT
/// column-index lookups).
pub(crate) fn column_index(schema: &TableSchema, name: &str) -> Option<usize> {
    schema
        .columns
        .iter()
        .position(|c| c.eq_ignore_ascii_case(name))
}

/// Compiles `expr` as a boolean condition: `true_target`/`false_target`
/// name where control continues on each outcome (a real jump label, or
/// "fall through to the next emitted instruction").
pub(crate) fn compile_cond(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    schema: &TableSchema,
    cursor: i32,
    expr: &Expr,
    true_target: Target,
    false_target: Target,
) -> Result<(), CodegenError> {
    match &expr.kind {
        ExprKind::Paren(inner) => {
            compile_cond(em, reg, schema, cursor, inner, true_target, false_target)
        }

        ExprKind::Unary {
            op: UnaryOp::Not,
            expr: inner,
        } => compile_cond(em, reg, schema, cursor, inner, false_target, true_target),

        ExprKind::Binary {
            op: BinaryOp::And,
            lhs,
            rhs,
        } => {
            // `false_target` must be a real label before `lhs` compiles
            // — if it were left as `Fallthrough`, "fall through" would
            // wrongly mean "continue into rhs's test code" (the next
            // thing physically emitted) rather than the AND's actual
            // false continuation, which only exists after `rhs` compiles.
            let (false_label, is_new) = ensure_label(em, false_target);
            compile_cond(
                em,
                reg,
                schema,
                cursor,
                lhs,
                Target::Fallthrough,
                Target::Jump(false_label),
            )?;
            compile_cond(
                em,
                reg,
                schema,
                cursor,
                rhs,
                true_target,
                Target::Jump(false_label),
            )?;
            if is_new {
                em.place(false_label);
            }
            Ok(())
        }

        ExprKind::Binary {
            op: BinaryOp::Or,
            lhs,
            rhs,
        } => {
            // Symmetric to `And` above: `true_target` must be a real
            // label before `lhs` compiles, or a `Fallthrough` true
            // would wrongly land in `rhs`'s test code instead of OR's
            // actual true continuation.
            let (true_label, is_new) = ensure_label(em, true_target);
            compile_cond(
                em,
                reg,
                schema,
                cursor,
                lhs,
                Target::Jump(true_label),
                Target::Fallthrough,
            )?;
            compile_cond(
                em,
                reg,
                schema,
                cursor,
                rhs,
                Target::Jump(true_label),
                false_target,
            )?;
            if is_new {
                em.place(true_label);
            }
            Ok(())
        }

        ExprKind::Binary { op, lhs, rhs }
            if matches!(
                op,
                BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
            ) =>
        {
            let collation = collation_of(lhs).or_else(|| collation_of(rhs));
            let l = compile_value(em, reg, schema, cursor, lhs)?;
            let r = compile_value(em, reg, schema, cursor, rhs)?;
            emit_compare_false_jump(em, *op, l, r, collation, true_target, false_target);
            Ok(())
        }

        ExprKind::Is { lhs, rhs, negated } => {
            // `a IS b`: true when both NULL, or both non-NULL and
            // equal — unlike `=`, never propagates NULL to "unknown".
            // No single opcode expresses this; compute it into a
            // 0/1 register first, then test truthiness like any other
            // value-mode boolean (LIKE/GLOB take the same shape).
            let (t, f) = if *negated {
                (false_target, true_target)
            } else {
                (true_target, false_target)
            };
            let l = compile_value(em, reg, schema, cursor, lhs)?;
            let r = compile_value(em, reg, schema, cursor, rhs)?;
            let result = reg.alloc();
            let both_null = em.new_label();
            let done = em.new_label();
            let addr = em.emit(Instruction::new(Opcode::IsNull, l, 0, 0));
            em.patch_p2(addr, both_null);

            let eq_true = em.new_label();
            let addr = em.emit(Instruction::new(Opcode::Eq, l, 0, r));
            em.patch_p2(addr, eq_true);
            em.emit(Instruction::new(Opcode::Integer, 0, result, 0));
            em.goto(done);
            em.place(eq_true);
            em.emit(Instruction::new(Opcode::Integer, 1, result, 0));
            em.goto(done);

            em.place(both_null);
            let r_null = em.new_label();
            let addr = em.emit(Instruction::new(Opcode::IsNull, r, 0, 0));
            em.patch_p2(addr, r_null);
            em.emit(Instruction::new(Opcode::Integer, 0, result, 0));
            em.goto(done);
            em.place(r_null);
            em.emit(Instruction::new(Opcode::Integer, 1, result, 0));

            em.place(done);
            finish_bool(em, t, f, |em, false_label| {
                let addr = em.emit(Instruction::new(Opcode::IfNot, result, 0, 1));
                em.patch_p2(addr, false_label);
            });
            Ok(())
        }

        ExprKind::IsNull {
            expr: inner,
            negated,
        } => {
            let r = compile_value(em, reg, schema, cursor, inner)?;
            // negated=false is `IS NULL` (condition true when NULL);
            // negated=true is `IS NOT NULL` (condition true when not
            // NULL). Emit the opcode matching "jump when condition is
            // false" so `finish_bool` sees a uniform false-jump
            // primitive.
            // negated=false (IS NULL): condition true iff NULL, so its
            // false-jump primitive fires when NOT null -> `NotNull`.
            // negated=true (IS NOT NULL): condition true iff not NULL,
            // so its false-jump primitive fires when NULL -> `IsNull`.
            let false_jump_op = if *negated {
                Opcode::IsNull
            } else {
                Opcode::NotNull
            };
            finish_bool(em, true_target, false_target, |em, false_label| {
                let addr = em.emit(Instruction::new(false_jump_op, r, 0, 0));
                em.patch_p2(addr, false_label);
            });
            Ok(())
        }

        ExprKind::Between {
            expr: inner,
            lo,
            hi,
            negated,
        } => {
            let (t, f) = if *negated {
                (false_target, true_target)
            } else {
                (true_target, false_target)
            };
            let ge_lo = Expr {
                kind: ExprKind::Binary {
                    op: BinaryOp::Ge,
                    lhs: inner.clone(),
                    rhs: lo.clone(),
                },
                span: expr.span,
            };
            let le_hi = Expr {
                kind: ExprKind::Binary {
                    op: BinaryOp::Le,
                    lhs: inner.clone(),
                    rhs: hi.clone(),
                },
                span: expr.span,
            };
            let (f_label, f_is_new) = ensure_label(em, f);
            compile_cond(
                em,
                reg,
                schema,
                cursor,
                &ge_lo,
                Target::Fallthrough,
                Target::Jump(f_label),
            )?;
            compile_cond(em, reg, schema, cursor, &le_hi, t, Target::Jump(f_label))?;
            if f_is_new {
                em.place(f_label);
            }
            Ok(())
        }

        ExprKind::In {
            expr: inner,
            list,
            negated,
        } => {
            let (t, f) = if *negated {
                (false_target, true_target)
            } else {
                (true_target, false_target)
            };
            if list.is_empty() {
                // `x IN ()` is always false.
                return compile_always_false(em, t, f);
            }
            let (t_label, t_is_new) = ensure_label(em, t);
            for (i, item) in list.iter().enumerate() {
                let eq = Expr {
                    kind: ExprKind::Binary {
                        op: BinaryOp::Eq,
                        lhs: inner.clone(),
                        rhs: Box::new(item.clone()),
                    },
                    span: expr.span,
                };
                if i.saturating_add(1) == list.len() {
                    compile_cond(em, reg, schema, cursor, &eq, Target::Jump(t_label), f)?;
                } else {
                    compile_cond(
                        em,
                        reg,
                        schema,
                        cursor,
                        &eq,
                        Target::Jump(t_label),
                        Target::Fallthrough,
                    )?;
                }
            }
            if t_is_new {
                em.place(t_label);
            }
            Ok(())
        }

        ExprKind::Like { .. } => {
            let r = compile_value(em, reg, schema, cursor, expr)?;
            finish_bool(em, true_target, false_target, |em, false_label| {
                // IfNot: jump to false_label when r is falsy OR NULL
                // (p3=1), matching three-valued WHERE semantics — NULL
                // excludes the row just like FALSE.
                let addr = em.emit(Instruction::new(Opcode::IfNot, r, 0, 1));
                em.patch_p2(addr, false_label);
            });
            Ok(())
        }

        // Any other expression used in boolean context (a bare column,
        // a function call, CASE, etc.): evaluate to a value and test
        // truthiness the same way as LIKE above.
        _ => {
            let r = compile_value(em, reg, schema, cursor, expr)?;
            finish_bool(em, true_target, false_target, |em, false_label| {
                let addr = em.emit(Instruction::new(Opcode::IfNot, r, 0, 1));
                em.patch_p2(addr, false_label);
            });
            Ok(())
        }
    }
}

/// `x IN ()` / other statically-false conditions: jump to `false_target`
/// (or fall through if it's already the fallthrough), never touching
/// `true_target`.
fn compile_always_false(
    em: &mut Emitter,
    _true_target: Target,
    false_target: Target,
) -> Result<(), CodegenError> {
    if let Target::Jump(label) = false_target {
        em.goto(label);
    }
    Ok(())
}

/// Given a primitive that emits a "jump to `false_label` when the
/// condition is false, fall through when true" instruction, resolves
/// the full `(true_target, false_target)` combination — inserting an
/// extra `Goto` when both are already real jump targets.
fn finish_bool(
    em: &mut Emitter,
    true_target: Target,
    false_target: Target,
    emit_false_jump: impl FnOnce(&mut Emitter, Label),
) {
    match (true_target, false_target) {
        (Target::Fallthrough, Target::Jump(f)) => emit_false_jump(em, f),
        (Target::Jump(t), Target::Fallthrough) => {
            let synth = em.new_label();
            emit_false_jump(em, synth);
            em.goto(t);
            em.place(synth);
        }
        (Target::Jump(t), Target::Jump(f)) => {
            emit_false_jump(em, f);
            em.goto(t);
        }
        (Target::Fallthrough, Target::Fallthrough) => {
            // Both branches continue at the same point — the condition
            // was evaluated only for a side effect (never emitted by
            // this module), so give false-jump a label that resolves to
            // "here" too.
            let synth = em.new_label();
            emit_false_jump(em, synth);
            em.place(synth);
        }
    }
}

/// Emits the appropriate compare opcode as a "jump to `false_label` on
/// false" primitive, then resolves `true_target`/`false_target` via
/// [`finish_bool`]. `Ne` has no dedicated opcode — it's `Eq`'s
/// complement, so its false-jump primitive is a plain `Eq` jump.
/// Resolves `target` to a real label usable as an immediate jump
/// destination, returning whether that label still needs `em.place`-ing
/// (i.e. it was freshly synthesized for a `Fallthrough` target rather
/// than an already-real `Jump`).
fn ensure_label(em: &mut Emitter, target: Target) -> (Label, bool) {
    match target {
        Target::Jump(l) => (l, false),
        Target::Fallthrough => (em.new_label(), true),
    }
}

/// Compiles a comparison as a jump. Always uses a true-jump-then-Goto
/// shape (never the tempting "jump on the complementary opcode"
/// shortcut): the compare opcodes (spec 009, Requirement 5) never jump
/// at all when either operand is NULL (`compare_jump`'s own three-
/// valued-logic contract), so a complement-based false-jump would
/// silently treat a NULL comparison as "condition holds" instead of
/// "condition's truth is unknown, which WHERE excludes just like
/// false" — this shape treats NULL and FALSE identically (both fail to
/// reach the true label, so both fall into the `Goto false_target`),
/// matching WHERE's own three-valued semantics for every comparator
/// uniformly, at the cost of one extra `Goto` versus the minimal
/// oracle-matching instruction count.
fn emit_compare_false_jump(
    em: &mut Emitter,
    op: BinaryOp,
    lhs: i32,
    rhs: i32,
    collation: Option<Collation>,
    true_target: Target,
    false_target: Target,
) {
    let p4 = collation.map_or(P4::None, p4_coll_seq);
    // `Ne` has no opcode of its own; it's `Eq` with true/false swapped.
    let (opcode, t, f) = match op {
        BinaryOp::Ne => (Opcode::Eq, false_target, true_target),
        BinaryOp::Eq => (Opcode::Eq, true_target, false_target),
        BinaryOp::Lt => (Opcode::Lt, true_target, false_target),
        BinaryOp::Le => (Opcode::Le, true_target, false_target),
        BinaryOp::Gt => (Opcode::Gt, true_target, false_target),
        BinaryOp::Ge => (Opcode::Ge, true_target, false_target),
        _ => unreachable!("caller only passes comparison operators"),
    };
    let (t_label, t_is_new) = ensure_label(em, t);
    let addr = em.emit(Instruction::with_p4(opcode, lhs, 0, rhs, p4));
    em.patch_p2(addr, t_label);
    if let Target::Jump(fl) = f {
        em.goto(fl);
    }
    if t_is_new {
        em.place(t_label);
    }
}

/// If `expr` is `x COLLATE name`, resolves `name` to a [`Collation`];
/// unrecognized collation names fall back to `None` (BINARY default).
fn collation_of(expr: &Expr) -> Option<Collation> {
    match &expr.kind {
        ExprKind::Collate { collation, .. } => match collation.to_ascii_uppercase().as_str() {
            "BINARY" => Some(Collation::Binary),
            "NOCASE" => Some(Collation::NoCase),
            "RTRIM" => Some(Collation::RTrim),
            _ => None,
        },
        ExprKind::Paren(inner) => collation_of(inner),
        _ => None,
    }
}

/// Compiles `expr` into a fresh register holding its value (value
/// mode) — used for result columns, function arguments, CASE branch
/// results, and as the operand feed for jump-mode comparisons.
pub(crate) fn compile_value(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    schema: &TableSchema,
    cursor: i32,
    expr: &Expr,
) -> Result<i32, CodegenError> {
    match &expr.kind {
        ExprKind::Paren(inner) => compile_value(em, reg, schema, cursor, inner),
        ExprKind::Collate { expr: inner, .. } => compile_value(em, reg, schema, cursor, inner),

        ExprKind::Literal(lit) => {
            let r = reg.alloc();
            match lit {
                Literal::Integer(i) => {
                    // `Opcode::Integer`'s P1 is i32-only (no P4-carried
                    // i64 immediate-load opcode exists in the frozen V2
                    // set) — a literal outside i32 range has no correct
                    // encoding here, so this errors rather than silently
                    // substituting a truncated value.
                    let p1 = i32::try_from(*i).map_err(|_| CodegenError::Unsupported {
                        reason: format!(
                            "integer literal {i} is out of range for this V2-scope compiler \
                             (no 64-bit immediate-load opcode in the frozen V2 set)"
                        ),
                    })?;
                    em.emit(Instruction::new(Opcode::Integer, p1, r, 0));
                }
                Literal::True => {
                    em.emit(Instruction::new(Opcode::Integer, 1, r, 0));
                }
                Literal::False => {
                    em.emit(Instruction::new(Opcode::Integer, 0, r, 0));
                }
                Literal::Str(s) => {
                    em.emit(Instruction::with_p4(
                        Opcode::String8,
                        0,
                        r,
                        0,
                        P4::Str(s.clone()),
                    ));
                }
                // No opcode in the frozen V2 set loads a REAL or BLOB
                // constant directly (SQLite's own OP_Real has no V2
                // counterpart here) — known simplification: represent
                // as text and rely on the value-semantics kernel's
                // text-to-numeric coercion at comparison/arithmetic
                // time. Exact literal round-tripping of REAL/BLOB
                // constants is out of this ticket's scope.
                Literal::Float(f) => {
                    em.emit(Instruction::with_p4(
                        Opcode::String8,
                        0,
                        r,
                        0,
                        P4::Str(format!("{f}")),
                    ));
                }
                Literal::Blob(bytes) => {
                    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                    em.emit(Instruction::with_p4(Opcode::String8, 0, r, 0, P4::Str(hex)));
                }
                Literal::Null => {} // Fresh registers already read as NULL.
            }
            Ok(r)
        }

        // Bound parameter values aren't supplied by this ticket's
        // compile-only entry point (no bind-value API yet) — compiles
        // to NULL (known simplification).
        ExprKind::Param(_) => Ok(reg.alloc()),

        ExprKind::Column { name, .. } => {
            let idx = column_index(schema, name)
                .ok_or_else(|| CodegenError::UnknownColumn { name: name.clone() })?;
            let r = reg.alloc();
            em.emit(Instruction::new(
                Opcode::Column,
                cursor,
                i32::try_from(idx).map_err(|_| CodegenError::Unsupported {
                    reason: format!("column index {idx} does not fit in a P2 operand"),
                })?,
                r,
            ));
            Ok(r)
        }

        ExprKind::FunctionCall { name, args, .. } => {
            let arg_exprs = match args {
                crate::parser::ast::FunctionArgs::Star => &[][..],
                crate::parser::ast::FunctionArgs::List(list) => list.as_slice(),
            };
            let first = reg.alloc_range(arg_exprs.len().max(1));
            for (i, arg) in arg_exprs.iter().enumerate() {
                let r = compile_value(em, reg, schema, cursor, arg)?;
                if r != first.saturating_add(i32::try_from(i).unwrap_or(i32::MAX)) {
                    // Bump allocator guarantees contiguity when args
                    // are compiled in order with no intervening
                    // allocation; this branch should not trigger.
                    return Err(CodegenError::Unsupported {
                        reason: "function argument registers were not contiguous".to_string(),
                    });
                }
            }
            let dest = reg.alloc();
            em.emit(Instruction::with_p4(
                Opcode::Function,
                0,
                first,
                dest,
                P4::Str(format!(
                    "{}({})",
                    name.to_ascii_lowercase(),
                    arg_exprs.len()
                )),
            ));
            Ok(dest)
        }

        ExprKind::Like {
            expr: inner,
            pattern,
            glob,
            negated,
            escape,
        } => {
            let (name, arity) = match escape {
                Some(_) if !*glob => ("like", 3),
                _ if *glob => ("glob", 2),
                _ => ("like", 2),
            };
            // Registry argument order is (pattern, text[, escape]) —
            // the reverse of SQL's `text LIKE pattern` syntax. Compile
            // operands in that order so the bump allocator hands out a
            // contiguous run matching `Function`'s expected layout.
            let pat_r = compile_value(em, reg, schema, cursor, pattern)?;
            let txt_r = compile_value(em, reg, schema, cursor, inner)?;
            if txt_r != pat_r.saturating_add(1) {
                return Err(CodegenError::Unsupported {
                    reason: "LIKE/GLOB text operand did not land in the register contiguous \
                             with its pattern operand"
                        .to_string(),
                });
            }
            if let Some(e) = escape {
                let esc_r = compile_value(em, reg, schema, cursor, e)?;
                if esc_r != pat_r.saturating_add(2) {
                    return Err(CodegenError::Unsupported {
                        reason: "LIKE ESCAPE operand did not land in the register contiguous \
                                 with its pattern/text operands"
                            .to_string(),
                    });
                }
            }
            let dest = reg.alloc();
            let p4 = P4::Str(format!("{name}({arity})"));
            em.emit(Instruction::with_p4(Opcode::Function, 0, pat_r, dest, p4));
            if *negated {
                let out = compile_negate_value(em, reg, dest);
                return Ok(out);
            }
            Ok(dest)
        }

        ExprKind::Unary { op, expr: inner } => match op {
            UnaryOp::Plus => compile_value(em, reg, schema, cursor, inner),
            UnaryOp::Minus => {
                let r = compile_value(em, reg, schema, cursor, inner)?;
                let zero = reg.alloc();
                em.emit(Instruction::new(Opcode::Integer, 0, zero, 0));
                let dest = reg.alloc();
                // Subtract: r[P3] = r[P2] - r[P1] -> 0 - r = -r via
                // P1=r, P2=zero.
                em.emit(Instruction::new(Opcode::Subtract, r, zero, dest));
                Ok(dest)
            }
            UnaryOp::Not => compile_bool_to_value(em, reg, schema, cursor, inner, true),
            // No bitwise-NOT opcode exists in the frozen V2 set —
            // known gap; passes the operand through unchanged rather
            // than inventing a new opcode.
            UnaryOp::BitNot => compile_value(em, reg, schema, cursor, inner),
        },

        ExprKind::Binary { op, lhs, rhs }
            if matches!(
                op,
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod
            ) =>
        {
            let l = compile_value(em, reg, schema, cursor, lhs)?;
            let r = compile_value(em, reg, schema, cursor, rhs)?;
            let dest = reg.alloc();
            let opcode = match op {
                BinaryOp::Add => Opcode::Add,
                BinaryOp::Sub => Opcode::Subtract,
                BinaryOp::Mul => Opcode::Multiply,
                BinaryOp::Div => Opcode::Divide,
                BinaryOp::Mod => Opcode::Remainder,
                _ => unreachable!(),
            };
            // Subtract/Divide/Remainder read as `r[P2] <op> r[P1]`
            // (SQLite's own operand order, per arithmetic.rs) — pass
            // (rhs=P1, lhs=P2) so `lhs <op> rhs` is what's computed.
            match opcode {
                Opcode::Subtract | Opcode::Divide | Opcode::Remainder => {
                    em.emit(Instruction::new(opcode, r, l, dest));
                }
                _ => {
                    em.emit(Instruction::new(opcode, l, r, dest));
                }
            }
            Ok(dest)
        }

        // No dedicated opcode exists for bitwise AND/OR/shift or `||`
        // concatenation in the frozen V2 set (spec 009's 52-opcode
        // inventory has no such category) — known gap, compiles to
        // NULL rather than inventing a new opcode.
        ExprKind::Binary { .. } => {
            let r = reg.alloc();
            Ok(r)
        }

        ExprKind::Cast {
            expr: inner,
            type_name,
        } => {
            let r = compile_value(em, reg, schema, cursor, inner)?;
            match type_name.to_ascii_uppercase() {
                t if t.contains("INT") => {
                    em.emit(Instruction::new(Opcode::MustBeInt, r, 0, 0));
                    Ok(r)
                }
                t if t.contains("REAL") || t.contains("FLOA") || t.contains("DOUB") => {
                    em.emit(Instruction::new(Opcode::RealAffinity, r, 0, 0));
                    Ok(r)
                }
                // TEXT/BLOB/NUMERIC CAST targets: no dedicated opcode
                // exists to force those affinities standalone (only
                // RealAffinity is in the frozen set) — known gap,
                // passes the value through unchanged.
                _ => Ok(r),
            }
        }

        ExprKind::Case {
            operand,
            whens,
            else_,
        } => {
            let dest = reg.alloc();
            let end_label = em.new_label();
            for (when_expr, then_expr) in whens {
                let next_label = em.new_label();
                let cond = match operand {
                    Some(op_expr) => Expr {
                        kind: ExprKind::Binary {
                            op: BinaryOp::Eq,
                            lhs: op_expr.clone(),
                            rhs: Box::new(when_expr.clone()),
                        },
                        span: when_expr.span,
                    },
                    None => when_expr.clone(),
                };
                compile_cond(
                    em,
                    reg,
                    schema,
                    cursor,
                    &cond,
                    Target::Fallthrough,
                    Target::Jump(next_label),
                )?;
                emit_branch_into(em, schema, cursor, then_expr, dest)?;
                em.goto(end_label);
                em.place(next_label);
            }
            // `dest` is a register slot the scan loop reuses every
            // iteration — a prior row's CASE result would otherwise
            // leak into this row's output when no WHEN matches and
            // there's no ELSE (registers don't reset between loop
            // iterations), so the no-match path always explicitly
            // (re)writes NULL rather than relying on "never written".
            match else_ {
                Some(else_expr) => emit_branch_into(em, schema, cursor, else_expr, dest)?,
                None => {
                    // No dedicated "load NULL" opcode exists; an
                    // out-of-range `Column` read reliably yields NULL
                    // (`cursor.rs`'s `column` doc: unlisted indices
                    // read as NULL) regardless of the cursor's real
                    // schema width, so this is used purely as a NULL
                    // source, not a real column read.
                    let sentinel_index =
                        i32::try_from(schema.columns.len().saturating_add(1)).unwrap_or(i32::MAX);
                    em.emit(Instruction::new(
                        Opcode::Column,
                        cursor,
                        sentinel_index,
                        dest,
                    ));
                }
            }
            em.place(end_label);
            Ok(dest)
        }

        // Boolean-valued expressions used in a value context (e.g. `a
        // = b` as a result column) materialize 0/1 via the jump-mode
        // compiler, matching Requirement 11's shape even when the
        // condition's answer must land in a register.
        ExprKind::Is { .. }
        | ExprKind::IsNull { .. }
        | ExprKind::Between { .. }
        | ExprKind::In { .. } => compile_bool_to_value(em, reg, schema, cursor, expr, false),
    }
}

/// `IfNot`-based boolean negation of an already-computed truthy value
/// register (used by `NOT LIKE`/`NOT GLOB`), materializing 0/1 into a
/// fresh register.
fn compile_negate_value(em: &mut Emitter, reg: &mut RegAlloc, src: i32) -> i32 {
    let out = reg.alloc();
    let true_label = em.new_label();
    let end_label = em.new_label();
    let addr = em.emit(Instruction::new(Opcode::IfNot, src, 0, 1));
    em.patch_p2(addr, true_label);
    em.emit(Instruction::new(Opcode::Integer, 0, out, 0));
    em.goto(end_label);
    em.place(true_label);
    em.emit(Instruction::new(Opcode::Integer, 1, out, 0));
    em.place(end_label);
    out
}

/// No `Copy`/`Move` opcode exists in the frozen V2 set, so CASE's
/// branch results (each computed into its own register) must land in
/// one shared destination. Known simplification: only `Literal` and
/// `Column` branch expressions are re-emitted directly into `dest`
/// (covers the V2 corpus's actual CASE usage, e.g.
/// `tests/corpus/sql/valid_in_subset/functions_case_cast.sql`'s
/// literal-only THEN/ELSE clauses); any other branch expression is
/// rejected outright — evaluating it into a temporary and leaving
/// `dest` untouched would silently surface a stale register from a
/// prior branch or a prior loop iteration as this branch's result, a
/// wrong-answer bug rather than a documented limitation. A future
/// ticket needs a real MOVE opcode to close this gap generally.
fn emit_branch_into(
    em: &mut Emitter,
    schema: &TableSchema,
    cursor: i32,
    expr: &Expr,
    dest: i32,
) -> Result<(), CodegenError> {
    match &expr.kind {
        ExprKind::Literal(Literal::Integer(i)) => {
            let p1 = i32::try_from(*i).map_err(|_| CodegenError::Unsupported {
                reason: format!(
                    "integer literal {i} is out of range for this V2-scope compiler \
                     (no 64-bit immediate-load opcode in the frozen V2 set)"
                ),
            })?;
            em.emit(Instruction::new(Opcode::Integer, p1, dest, 0));
        }
        ExprKind::Literal(Literal::True) => {
            em.emit(Instruction::new(Opcode::Integer, 1, dest, 0));
        }
        ExprKind::Literal(Literal::False) => {
            em.emit(Instruction::new(Opcode::Integer, 0, dest, 0));
        }
        ExprKind::Literal(Literal::Str(s)) => {
            em.emit(Instruction::with_p4(
                Opcode::String8,
                0,
                dest,
                0,
                P4::Str(s.clone()),
            ));
        }
        ExprKind::Literal(Literal::Null) => {}
        ExprKind::Column { name, .. } => {
            let idx = column_index(schema, name)
                .ok_or_else(|| CodegenError::UnknownColumn { name: name.clone() })?;
            em.emit(Instruction::new(
                Opcode::Column,
                cursor,
                i32::try_from(idx).map_err(|_| CodegenError::Unsupported {
                    reason: format!("column index {idx} does not fit in a P2 operand"),
                })?,
                dest,
            ));
        }
        _ => {
            return Err(CodegenError::Unsupported {
                reason: "CASE branch results other than a bare literal or column reference are \
                         not yet supported by this V2-scope compiler (no MOVE opcode to copy a \
                         computed value into the CASE's shared result register)"
                    .to_string(),
            });
        }
    }
    Ok(())
}

fn compile_bool_to_value(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    schema: &TableSchema,
    cursor: i32,
    expr: &Expr,
    negate: bool,
) -> Result<i32, CodegenError> {
    let dest = reg.alloc();
    let true_label = em.new_label();
    let end_label = em.new_label();
    let (t, f) = if negate {
        (Target::Fallthrough, Target::Jump(true_label))
    } else {
        (Target::Jump(true_label), Target::Fallthrough)
    };
    compile_cond(em, reg, schema, cursor, expr, t, f)?;
    em.emit(Instruction::new(Opcode::Integer, 0, dest, 0));
    em.goto(end_label);
    em.place(true_label);
    em.emit(Instruction::new(Opcode::Integer, 1, dest, 0));
    em.place(end_label);
    Ok(dest)
}
