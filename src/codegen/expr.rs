//! Expression lowering (spec 009, Requirement 11): boolean-valued
//! expressions compile to jump instructions targeting a true/false
//! continuation, never an intermediate boolean register — the classic
//! jumping-code-generation technique. `compile_cond` is the jump-mode
//! entry point; `compile_value` is the ordinary register-producing
//! entry point used for result columns, function arguments, and CASE
//! branch results.
//!
//! Every column reference resolves through a [`Scope`] (#237) rather
//! than a bare `schema: &TableSchema, cursor: i32` pair — the single-
//! table V2 case is just `Scope::single(schema, cursor)`; a join chain
//! is `Scope` with one [`crate::codegen::TableBinding`] per joined
//! table, and `table.column`/bare `column` references resolve against
//! whichever binding matches (see `Scope::resolve`'s doc comment for
//! the alias-vs-name precedence rule).

use crate::codegen::{
    p4_coll_seq, CodegenError, CondTargets, Emitter, Label, NullTarget, RegAlloc, Scope, Target,
};
use crate::parser::ast::{BinaryOp, Expr, ExprKind, Literal, ParamKind, UnaryOp};
use crate::schema::{rowid_alias_column, TableSchema};
use crate::vdbe::{affinity_of, comparison_affinity, Affinity, Collation, Instruction, Opcode, P4};

/// Resolves a bare `Expr::Column` name against a single schema; any
/// other expression is a codegen error only when a caller specifically
/// requires a plain column (there is no such requirement in this
/// module — kept for callers like `select.rs`'s ORDER BY/DISTINCT
/// column-index lookups, and [`crate::codegen::Scope::resolve`]'s own
/// per-binding lookup).
pub(crate) fn column_index(schema: &TableSchema, name: &str) -> Option<usize> {
    schema
        .columns
        .iter()
        .position(|c| c.eq_ignore_ascii_case(name))
}

/// Compiles `expr` as a boolean condition. [`CondTargets`] says where
/// control continues on each of the three outcomes: `on_true`/
/// `on_false` are real jump labels (or "fall through to the next
/// emitted instruction"), and `on_null` names which of those two the
/// *unknown* outcome joins — SQL is three-valued, and a jump has two
/// destinations.
///
/// Callers that want SQL's `WHERE` semantics use
/// [`CondTargets::null_is_false`]: a predicate whose truth is unknown
/// excludes the row exactly like a false one. What they must NOT do is
/// assume that stays true under negation — `NOT unknown` is still
/// unknown, so [`CondTargets::negate`] swaps the two targets *and*
/// flips `on_null`, leaving the unknown outcome on the address it
/// already had (#134).
pub(crate) fn compile_cond(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    scope: &Scope,
    expr: &Expr,
    targets: CondTargets,
) -> Result<(), CodegenError> {
    match &expr.kind {
        ExprKind::Paren(inner) => compile_cond(em, reg, scope, inner, targets),

        // Swapping the targets is right — it is what SQLite's own
        // `sqlite3ExprIfTrue`/`sqlite3ExprIfFalse` pair does for
        // `TK_NOT` — but only once `on_null` comes along for the
        // ride. Flipping it keeps the unknown outcome on the same
        // address across the swap; without that (the #134 bug) NULL
        // silently inherited whichever target had just become "false",
        // i.e. the keep-the-row one, and `WHERE NOT (x = 5)` returned
        // rows where `x IS NULL`.
        ExprKind::Unary {
            op: UnaryOp::Not,
            expr: inner,
        } => compile_cond(em, reg, scope, inner, targets.negate()),

        ExprKind::Binary {
            op: BinaryOp::And,
            lhs,
            rhs,
        } => {
            // `on_false` must be a real label before `lhs` compiles
            // — if it were left as `Fallthrough`, "fall through" would
            // wrongly mean "continue into rhs's test code" (the next
            // thing physically emitted) rather than the AND's actual
            // false continuation, which only exists after `rhs` compiles.
            //
            // `on_null` passes to both operands unchanged, and that
            // is exactly three-valued AND, both ways round. With
            // `NullTarget::False`: an unknown `lhs` goes straight to
            // the false continuation, which is right because every
            // completion of `unknown AND rhs` (false, or unknown) lands
            // there too. With `NullTarget::True`: an unknown `lhs`
            // falls through into `rhs`'s test, which is also right —
            // `unknown AND false` is false and reaches `false_label`
            // via `rhs`, while `unknown AND true`/`unknown AND unknown`
            // are unknown and reach `on_true`, where NULL belongs
            // under this setting.
            let (false_label, is_new) = ensure_label(em, targets.on_false);
            let operand = targets.with_false(Target::Jump(false_label));
            compile_cond(em, reg, scope, lhs, operand.with_true(Target::Fallthrough))?;
            compile_cond(em, reg, scope, rhs, operand)?;
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
            // Symmetric to `And` above: `on_true` must be a real
            // label before `lhs` compiles, or a `Fallthrough` true
            // would wrongly land in `rhs`'s test code instead of OR's
            // actual true continuation. `on_null` threads through
            // unchanged for the mirror-image reason: under
            // `NullTarget::True` an unknown `lhs` jumps straight to
            // `true_label` (every completion of `unknown OR rhs` is
            // true or unknown, and both belong there), and under
            // `NullTarget::False` it falls into `rhs`, which decides
            // between true and the false/unknown continuation.
            let (true_label, is_new) = ensure_label(em, targets.on_true);
            let operand = targets.with_true(Target::Jump(true_label));
            compile_cond(em, reg, scope, lhs, operand.with_false(Target::Fallthrough))?;
            compile_cond(em, reg, scope, rhs, operand)?;
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
            let affinity =
                comparison_affinity(expr_affinity(scope, lhs), expr_affinity(scope, rhs));
            let l = compile_value(em, reg, scope, lhs)?;
            let r = compile_value(em, reg, scope, rhs)?;
            emit_compare_false_jump(em, *op, l, r, collation, affinity, targets)
        }

        ExprKind::Is { lhs, rhs, negated } => {
            // `a IS b`: true when both NULL, or both non-NULL and
            // equal — unlike `=`, never propagates NULL to "unknown".
            // No single opcode expresses this; compute it into a
            // 0/1 register first, then test truthiness like any other
            // value-mode boolean (LIKE/GLOB take the same shape).
            // `on_null` is deliberately ignored here and in the
            // `IsNull` arm below: these two are the only conditions in
            // SQL that are always definitely true or definitely false,
            // so they have no unknown outcome to route, and swapping
            // the targets for `negated` is sound without flipping it.
            let (t, f) = if *negated {
                (targets.on_false, targets.on_true)
            } else {
                (targets.on_true, targets.on_false)
            };
            let l = compile_value(em, reg, scope, lhs)?;
            let r = compile_value(em, reg, scope, rhs)?;
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
            let r = compile_value(em, reg, scope, inner)?;
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
            finish_bool(em, targets.on_true, targets.on_false, |em, false_label| {
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
            // Lowered per SQLite's own rule: `x BETWEEN lo AND hi` is
            // `x >= lo AND x <= hi`, and `x NOT BETWEEN lo AND hi` is
            // `x < lo OR x > hi` — NOT the same shape with true/false
            // swapped. Swapping targets would make a NULL `x` (where
            // neither comparison jumps at all, per
            // `emit_compare_false_jump`'s three-valued contract) come
            // out *true* for `NOT BETWEEN`, when the honest answer is
            // "unknown", which WHERE excludes just like false.
            let cmp = |op, rhs: &Expr| Expr {
                kind: ExprKind::Binary {
                    op,
                    lhs: inner.clone(),
                    rhs: Box::new(rhs.clone()),
                },
                span: expr.span,
            };

            if *negated {
                let lt_lo = cmp(BinaryOp::Lt, lo);
                let gt_hi = cmp(BinaryOp::Gt, hi);
                let (t_label, t_is_new) = ensure_label(em, targets.on_true);
                let arm = targets.with_true(Target::Jump(t_label));
                compile_cond(em, reg, scope, &lt_lo, arm.with_false(Target::Fallthrough))?;
                compile_cond(em, reg, scope, &gt_hi, arm)?;
                if t_is_new {
                    em.place(t_label);
                }
            } else {
                let ge_lo = cmp(BinaryOp::Ge, lo);
                let le_hi = cmp(BinaryOp::Le, hi);
                let (f_label, f_is_new) = ensure_label(em, targets.on_false);
                let arm = targets.with_false(Target::Jump(f_label));
                compile_cond(em, reg, scope, &ge_lo, arm.with_true(Target::Fallthrough))?;
                compile_cond(em, reg, scope, &le_hi, arm)?;
                if f_is_new {
                    em.place(f_label);
                }
            }
            Ok(())
        }

        ExprKind::In {
            expr: inner,
            list,
            negated,
        } => {
            if list.is_empty() {
                // `x IN ()` is always false, even for a NULL `x` — an
                // empty list leaves nothing to be uncertain against.
                let (t, f) = if *negated {
                    (targets.on_false, targets.on_true)
                } else {
                    (targets.on_true, targets.on_false)
                };
                return compile_always_false(em, t, f);
            }

            // `IN`'s three outcomes — a definite match, a definite
            // non-match, or "unknown" (no match found, but `inner` or
            // some list item was NULL along the way) — don't collapse
            // to a single true/false jump the way other comparisons
            // do: `NOT IN`'s definite-non-match and unknown outcomes
            // diverge (`NOT FALSE` = true, `NOT NULL` = still NULL),
            // so a per-item comparison can't just swap targets like
            // `emit_compare_false_jump` does. `saw_null` is a small
            // exception to this module's "never an intermediate
            // boolean register" rule (shared with the `Is`/`IsNot`
            // handling above), needed to remember that exception past
            // the loop that discovers it.
            let l = compile_value(em, reg, scope, inner)?;
            let saw_null = reg.alloc();
            em.emit(Instruction::new(Opcode::Integer, 0, saw_null, 0));

            let (true_label, true_is_new) = ensure_label(em, targets.on_true);
            let (false_label, false_is_new) = ensure_label(em, targets.on_false);
            // A match found means IN is true; exhausting the list
            // without one means IN is false — `negated` (`NOT IN`)
            // swaps which final label each of those routes to. An
            // unknown outcome, below, always routes to `false_label`
            // regardless of `negated`: NULL is never true.
            let (found_label, unmatched_label) = if *negated {
                (false_label, true_label)
            } else {
                (true_label, false_label)
            };
            // `negated` does not move the unknown outcome — `NOT
            // unknown` is still unknown — so it routes by `on_null`
            // alone, exactly like every other arm (#134). Before that
            // ticket this was hardcoded to `false_label`, which was
            // right only because `WHERE` was the sole caller.
            let null_label = match targets.on_null {
                NullTarget::True => true_label,
                NullTarget::False => false_label,
            };

            let inner_null_addr = em.emit(Instruction::new(Opcode::IsNull, l, 0, 0));
            em.patch_p2(inner_null_addr, null_label);

            for item in list.iter() {
                let collation = collation_of(inner).or_else(|| collation_of(item));
                let affinity =
                    comparison_affinity(expr_affinity(scope, inner), expr_affinity(scope, item));
                let p4 = p4_coll_seq(collation.unwrap_or(Collation::Binary), affinity);
                let r = compile_value(em, reg, scope, item)?;

                let item_null_label = em.new_label();
                let skip_label = em.new_label();
                let addr = em.emit(Instruction::new(Opcode::IsNull, r, 0, 0));
                em.patch_p2(addr, item_null_label);
                let addr = em.emit(Instruction::with_p4(Opcode::Eq, l, 0, r, p4));
                em.patch_p2(addr, found_label);
                em.goto(skip_label);

                em.place(item_null_label);
                em.emit(Instruction::new(Opcode::Integer, 1, saw_null, 0));
                em.place(skip_label);
            }

            // Exhausted the list without a match: route to
            // `unmatched_label` only if every comparison was a clean
            // non-match (`saw_null` still 0); otherwise at least one
            // comparison was against NULL, so the honest answer is
            // "unknown", which goes wherever `on_null` says.
            let addr = em.emit(Instruction::new(Opcode::IfNot, saw_null, 0, 0));
            em.patch_p2(addr, unmatched_label);
            em.goto(null_label);

            if false_is_new {
                em.place(false_label);
            }
            if true_is_new {
                em.place(true_label);
            }
            Ok(())
        }

        ExprKind::Like { .. } => {
            let r = compile_value(em, reg, scope, expr)?;
            finish_truthy(em, r, targets);
            Ok(())
        }

        // #238: EXISTS/NOT EXISTS never has an unknown outcome (see
        // `subquery::compile_exists`'s doc comment), so `targets` is
        // handed through as-is rather than materialized via
        // `compile_bool_to_value`.
        ExprKind::Exists { subquery, negated } => {
            crate::codegen::subquery::compile_exists(em, reg, scope, subquery, *negated, targets)
        }

        ExprKind::InSubquery {
            expr: inner,
            subquery,
            negated,
        } => crate::codegen::subquery::compile_in_subquery(
            em, reg, scope, inner, subquery, *negated, targets,
        ),

        ExprKind::InSubqueryMulti {
            exprs,
            subquery,
            negated,
        } => crate::codegen::subquery::compile_in_subquery_multi(
            em, reg, scope, exprs, subquery, *negated, targets,
        ),

        // A scalar subquery used directly as a boolean condition (e.g.
        // `WHERE (SELECT x FROM t)`): compute its value, then test
        // truthiness like any other value-mode boolean.
        ExprKind::Subquery(_) => {
            let r = compile_value(em, reg, scope, expr)?;
            finish_truthy(em, r, targets);
            Ok(())
        }

        // Any other expression used in boolean context (a bare column,
        // a function call, CASE, etc.): evaluate to a value and test
        // truthiness the same way as LIKE above.
        _ => {
            let r = compile_value(em, reg, scope, expr)?;
            finish_truthy(em, r, targets);
            Ok(())
        }
    }
}

/// Tests an already-computed value register for truthiness as a
/// three-valued condition. `IfNot`'s `P3` flag folds NULL into the
/// false jump, which covers `NullTarget::False` in one instruction;
/// the other setting needs an explicit `IsNull` probe first, because
/// no single opcode routes NULL and falsy to *different* addresses.
fn finish_truthy(em: &mut Emitter, r: i32, targets: CondTargets) {
    match targets.on_null {
        NullTarget::False => {
            finish_bool(em, targets.on_true, targets.on_false, |em, false_label| {
                let addr = em.emit(Instruction::new(Opcode::IfNot, r, 0, 1));
                em.patch_p2(addr, false_label);
            });
        }
        NullTarget::True => {
            let (t_label, t_is_new) = ensure_label(em, targets.on_true);
            let addr = em.emit(Instruction::new(Opcode::IsNull, r, 0, 0));
            em.patch_p2(addr, t_label);
            // NULL is already gone, so `IfNot`'s P3 stays 0 and the
            // remaining test is the plain two-valued one.
            finish_bool(
                em,
                Target::Jump(t_label),
                targets.on_false,
                |em, false_label| {
                    let addr = em.emit(Instruction::new(Opcode::IfNot, r, 0, 0));
                    em.patch_p2(addr, false_label);
                },
            );
            if t_is_new {
                em.place(t_label);
            }
        }
    }
}

/// `x IN ()` / other statically-false conditions: jump to the false
/// target (or fall through if it's already the fallthrough), never
/// touching the true one.
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
/// the full `(on_true, on_false)` combination — inserting an
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
pub(crate) fn ensure_label(em: &mut Emitter, target: Target) -> (Label, bool) {
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
    affinity: Affinity,
    targets: CondTargets,
) -> Result<(), CodegenError> {
    let p4 = p4_coll_seq(collation.unwrap_or(Collation::Binary), affinity);
    // `Ne` has no opcode of its own; it's `Eq` with true/false swapped
    // — and that swap needs `null_target` flipped with it for the same
    // reason `NOT` does (#134). Without the flip, `WHERE x <> 5`
    // returned rows where `x IS NULL`: the NULL comparison reached the
    // trailing `Goto`, which the swap had just repointed at the
    // keep-the-row target. The caller only ever passes a comparison
    // operator (guarded by its own `matches!` filter), so `Some` always
    // holds; a non-comparison op is a codegen-internal error, not a
    // reachable SQL-input case.
    let resolved = match op {
        BinaryOp::Ne => Some((Opcode::Eq, targets.negate())),
        BinaryOp::Eq => Some((Opcode::Eq, targets)),
        BinaryOp::Lt => Some((Opcode::Lt, targets)),
        BinaryOp::Le => Some((Opcode::Le, targets)),
        BinaryOp::Gt => Some((Opcode::Gt, targets)),
        BinaryOp::Ge => Some((Opcode::Ge, targets)),
        _ => None,
    };
    let Some((opcode, targets)) = resolved else {
        return Err(CodegenError::Unsupported {
            reason: "emit_compare_false_jump called with a non-comparison operator".to_string(),
        });
    };
    let (t_label, t_is_new) = ensure_label(em, targets.on_true);
    // A NULL operand makes the compare opcode not jump at all, so it
    // otherwise always lands on `f`. When the unknown outcome belongs
    // with `t` instead, probe for it explicitly first — SQLite spells
    // the same thing as the `SQLITE_JUMPIFNULL` bit in the compare
    // instruction's P5, which this instruction format does not carry.
    if targets.on_null == NullTarget::True {
        let addr = em.emit(Instruction::new(Opcode::IsNull, lhs, 0, 0));
        em.patch_p2(addr, t_label);
        let addr = em.emit(Instruction::new(Opcode::IsNull, rhs, 0, 0));
        em.patch_p2(addr, t_label);
    }
    let addr = em.emit(Instruction::with_p4(opcode, lhs, 0, rhs, p4));
    em.patch_p2(addr, t_label);
    if let Target::Jump(fl) = targets.on_false {
        em.goto(fl);
    }
    if t_is_new {
        em.place(t_label);
    }
    Ok(())
}

/// Reads column `idx` of the row at `cursor` into `dest`, emitting
/// `Rowid` rather than `Column` for a rowid-alias column. A table's
/// `INTEGER PRIMARY KEY` column is stored as a NULL placeholder in
/// every record (spike 003 finding 1) — reading it with `Column` yields
/// NULL, so `SELECT x FROM t WHERE x=2` silently matched nothing until
/// this substitution existed. `src/dump.rs` has always done the same
/// thing; this is the compiled read path catching up.
///
/// Every column read in the compiled path must come through here.
/// `select.rs`'s result-column expansion emitted a bare `Column`
/// instead, which is why `SELECT *` still answered NULL for an
/// `INTEGER PRIMARY KEY` long after `SELECT id` was fixed.
pub(crate) fn emit_column_read(
    em: &mut Emitter,
    schema: &TableSchema,
    cursor: i32,
    idx: usize,
    dest: i32,
) -> Result<(), CodegenError> {
    if rowid_alias_column(schema) == Some(idx) {
        em.emit(Instruction::new(Opcode::Rowid, cursor, dest, 0));
        return Ok(());
    }
    em.emit(Instruction::new(
        Opcode::Column,
        cursor,
        i32::try_from(idx).map_err(|_| CodegenError::Unsupported {
            reason: format!("column index {idx} does not fit in a P2 operand"),
        })?,
        dest,
    ));
    // SQLite's on-disk format may store a REAL value using the integer-0/1
    // serial type optimization (file format doc, serial types 8/9) when
    // the value is losslessly an integer — independent of the column's
    // declared affinity. Real SQLite's OP_Column for a REAL-affinity
    // column is always followed by OP_RealAffinity to undo that
    // optimization on read; without it, `SELECT r FROM t` for a REAL
    // column holding `0.0` answered `0` instead of `0.0` (#143).
    if schema
        .column_types
        .get(idx)
        .is_some_and(|t| affinity_of(t) == Affinity::Real)
    {
        em.emit(Instruction::new(Opcode::RealAffinity, dest, 0, 0));
    }
    Ok(())
}

/// Whether this call is one of SQLite's built-in aggregates
/// (`func.c`'s aggregate registry). `max`/`min` are overloaded: the
/// one-argument form is the aggregate, but `max(a, b)` is an ordinary
/// scalar function, so arity — not the name alone — decides.
pub(crate) fn is_aggregate_call(name: &str, args: &crate::parser::ast::FunctionArgs) -> bool {
    let arity = match args {
        crate::parser::ast::FunctionArgs::Star => 0,
        crate::parser::ast::FunctionArgs::List(list) => list.len(),
    };
    match name.to_ascii_lowercase().as_str() {
        "avg" | "count" | "group_concat" | "string_agg" | "sum" | "total" => true,
        "max" | "min" => arity <= 1,
        _ => false,
    }
}

/// An expression's own affinity, per SQLite's `sqlite3ExprAffinity`
/// (spec 008 Requirement 1's comparison-affinity half, #138): a bare
/// column carries its declared-type affinity, a `CAST` carries its
/// target type's affinity, and a parenthesized expression defers to
/// its inner expression. Every other expression (literals, function
/// calls, arithmetic) has no affinity of its own — matching SQLite,
/// where only columns and casts do.
pub(crate) fn expr_affinity(scope: &Scope, expr: &Expr) -> Option<Affinity> {
    match &expr.kind {
        ExprKind::Column { table, name, .. } => {
            let (_, idx, schema, _) = scope.resolve(table.as_deref(), name).ok()?;
            let declared = schema.column_types.get(idx)?;
            Some(affinity_of(declared))
        }
        ExprKind::Cast { type_name, .. } => Some(affinity_of(type_name)),
        ExprKind::Paren(inner) => expr_affinity(scope, inner),
        _ => None,
    }
}

/// If `expr` is `x COLLATE name`, resolves `name` to a [`Collation`];
/// unrecognized collation names fall back to `None` (BINARY default).
pub(crate) fn collation_of(expr: &Expr) -> Option<Collation> {
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
    scope: &Scope,
    expr: &Expr,
) -> Result<i32, CodegenError> {
    match &expr.kind {
        ExprKind::Paren(inner) => compile_value(em, reg, scope, inner),
        ExprKind::Collate { expr: inner, .. } => compile_value(em, reg, scope, inner),

        ExprKind::Literal(lit) => {
            let r = reg.alloc();
            match lit {
                Literal::Integer(i) => {
                    // #142: `Opcode::Integer`'s P1 is i32-only, so a
                    // literal outside that range (but within i64) loads
                    // via `Int64`'s P4-carried i64 immediate instead —
                    // the harvested 64-bit counterpart, not a codegen
                    // error.
                    match i32::try_from(*i) {
                        Ok(p1) => {
                            em.emit(Instruction::new(Opcode::Integer, p1, r, 0));
                        }
                        Err(_) => {
                            em.emit(Instruction::with_p4(Opcode::Int64, 0, r, 0, P4::Int(*i)));
                        }
                    }
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
                // #142: a real literal loads as an actual `Value::Real`
                // via the harvested `Real` opcode, not `String8` text
                // relying on coercion at comparison/arithmetic time
                // (the #138 bug this used to cause).
                Literal::Float(f) => {
                    em.emit(Instruction::with_p4(Opcode::Real, 0, r, 0, P4::Real(*f)));
                }
                // #142: a blob literal loads as an actual `Value::Blob`
                // via the harvested `Blob` opcode — hex-text never
                // actually coerced back to a blob (BLOB affinity never
                // converts text to blob, matching SQLite), so
                // `WHERE b = x'41'` always failed under the old scheme.
                Literal::Blob(bytes) => {
                    let len =
                        i32::try_from(bytes.len()).map_err(|_| CodegenError::Unsupported {
                            reason: format!(
                                "blob literal of {} bytes does not fit in a P1 operand",
                                bytes.len()
                            ),
                        })?;
                    em.emit(Instruction::with_p4(
                        Opcode::Blob,
                        len,
                        r,
                        0,
                        P4::Blob(bytes.clone()),
                    ));
                }
                Literal::Null => {} // Fresh registers already read as NULL.
            }
            Ok(r)
        }

        // `?` and `?NNN` compile to `Variable`, reading whatever the
        // caller bound via `Vm::bind_params`/`execute_with_params`
        // (#137). Named forms (`:name`/`@name`/`$name`) aren't wired to
        // an index yet — out of #137's bounded scope — so they still
        // compile to an always-NULL register (known simplification,
        // same as before).
        ExprKind::Param(kind) => {
            let r = reg.alloc();
            let index = match kind {
                ParamKind::Anonymous => Some(reg.anonymous_param()),
                ParamKind::Numbered(n) => Some(reg.numbered_param(*n)),
                ParamKind::Colon(_) | ParamKind::At(_) | ParamKind::Dollar(_) => None,
            };
            if let Some(index) = index {
                let p1 = i32::try_from(index).map_err(|_| CodegenError::Unsupported {
                    reason: format!("parameter index {index} is out of range"),
                })?;
                em.emit(Instruction::new(Opcode::Variable, p1, r, 0));
            }
            Ok(r)
        }

        ExprKind::Column { table, name, .. } => {
            let (cursor, idx, schema, forced_null) = scope.resolve(table.as_deref(), name)?;
            let r = reg.alloc();
            if forced_null {
                // #237's LEFT JOIN null-extension: this binding has no
                // matching row (or `cursor` may not even be positioned
                // on live data at all), so every column reads as NULL
                // rather than going through a real `Column`/`Rowid`
                // read.
                em.emit(Instruction::new(Opcode::Null, 0, r, 0));
            } else {
                emit_column_read(em, schema, cursor, idx, r)?;
            }
            Ok(r)
        }

        ExprKind::FunctionCall { name, args, .. } => {
            // Aggregates need a grouping/accumulator pass this V2
            // compiler doesn't have. Rejecting them is not just a
            // missing-feature guard: compiling one as an ordinary
            // scalar `Function` call emits it *per row*, so
            // `SELECT count(*) FROM t` silently returns one row per
            // input row instead of a single count — wrong output is
            // worse than a refusal.
            if is_aggregate_call(name, args) {
                return Err(CodegenError::Unsupported {
                    reason: format!("aggregate function {}", name.to_ascii_lowercase()),
                });
            }
            let arg_exprs = match args {
                crate::parser::ast::FunctionArgs::Star => &[][..],
                crate::parser::ast::FunctionArgs::List(list) => list.as_slice(),
            };
            // `Function` reads its arguments from a contiguous register
            // window starting at P2, so the args must land next to each
            // other. Reserving the window up front and *then* compiling
            // into it does not work: `compile_value` allocates its own
            // destination, so every argument landed past the reservation.
            // Instead, compile the args first and take the window from
            // where they actually landed — consecutive simple args are
            // naturally adjacent this way. When that does not hold (an
            // argument whose own lowering allocates its destination
            // before its operands, e.g. `coalesce(i, -1)` alongside
            // another such call), fall back to copying each arg into a
            // freshly reserved contiguous run (#141).
            let mut arg_regs = Vec::with_capacity(arg_exprs.len());
            for arg in arg_exprs.iter() {
                arg_regs.push(compile_value(em, reg, scope, arg)?);
            }
            let mut first = match arg_regs.first().copied() {
                Some(r) => r,
                // Zero-arg call (or `f(*)`): P2 still has to point at a
                // register, and nothing reads it.
                None => reg.alloc(),
            };
            let already_contiguous = arg_regs
                .iter()
                .enumerate()
                .all(|(i, &r)| r == first.saturating_add(i32::try_from(i).unwrap_or(i32::MAX)));
            if !already_contiguous {
                let dests: Vec<i32> = (0..arg_regs.len()).map(|_| reg.alloc()).collect();
                if let Some(&dest_first) = dests.first() {
                    first = dest_first;
                }
                for (&r, &dest) in arg_regs.iter().zip(&dests) {
                    em.emit(Instruction::new(Opcode::Copy, r, dest, 0));
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
            let pat_r = compile_value(em, reg, scope, pattern)?;
            let txt_r = compile_value(em, reg, scope, inner)?;
            if txt_r != pat_r.saturating_add(1) {
                return Err(CodegenError::Unsupported {
                    reason: "LIKE/GLOB text operand did not land in the register contiguous \
                             with its pattern operand"
                        .to_string(),
                });
            }
            if let Some(e) = escape {
                let esc_r = compile_value(em, reg, scope, e)?;
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
            UnaryOp::Plus => compile_value(em, reg, scope, inner),
            UnaryOp::Minus => {
                let r = compile_value(em, reg, scope, inner)?;
                let zero = reg.alloc();
                em.emit(Instruction::new(Opcode::Integer, 0, zero, 0));
                let dest = reg.alloc();
                // Subtract: r[P3] = r[P2] - r[P1] -> 0 - r = -r via
                // P1=r, P2=zero.
                em.emit(Instruction::new(Opcode::Subtract, r, zero, dest));
                Ok(dest)
            }
            // `Not` is the whole reason this is not routed through
            // `compile_bool_to_value`: it is the oracle's own lowering
            // for `SELECT NOT x` (one instruction, verified against the
            // pinned 3.53.4 `EXPLAIN`), and it propagates NULL in a
            // register, which jump-mode code cannot do at all.
            UnaryOp::Not => {
                let r = compile_value(em, reg, scope, inner)?;
                let dest = reg.alloc();
                em.emit(Instruction::new(Opcode::Not, r, dest, 0));
                Ok(dest)
            }
            UnaryOp::BitNot => {
                let r = compile_value(em, reg, scope, inner)?;
                let dest = reg.alloc();
                em.emit(Instruction::new(Opcode::BitNot, r, dest, 0));
                Ok(dest)
            }
        },

        ExprKind::Binary { op, lhs, rhs }
            if matches!(
                op,
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod
            ) =>
        {
            let l = compile_value(em, reg, scope, lhs)?;
            let r = compile_value(em, reg, scope, rhs)?;
            let dest = reg.alloc();
            // The caller's own `matches!` filter guarantees `op` is one
            // of these five; any other value is a codegen-internal
            // error, not a reachable SQL-input case.
            let opcode = match op {
                BinaryOp::Add => Opcode::Add,
                BinaryOp::Sub => Opcode::Subtract,
                BinaryOp::Mul => Opcode::Multiply,
                BinaryOp::Div => Opcode::Divide,
                BinaryOp::Mod => Opcode::Remainder,
                _ => {
                    return Err(CodegenError::Unsupported {
                        reason: "arithmetic lowering reached with a non-arithmetic operator"
                            .to_string(),
                    })
                }
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

        ExprKind::Binary { op, lhs, rhs }
            if matches!(
                op,
                BinaryOp::BitAnd
                    | BinaryOp::BitOr
                    | BinaryOp::Shl
                    | BinaryOp::Shr
                    | BinaryOp::Concat
            ) =>
        {
            let l = compile_value(em, reg, scope, lhs)?;
            let r = compile_value(em, reg, scope, rhs)?;
            let dest = reg.alloc();
            let opcode = match op {
                BinaryOp::BitAnd => Opcode::BitAnd,
                BinaryOp::BitOr => Opcode::BitOr,
                BinaryOp::Shl => Opcode::ShiftLeft,
                BinaryOp::Shr => Opcode::ShiftRight,
                BinaryOp::Concat => Opcode::Concat,
                _ => {
                    return Err(CodegenError::Unsupported {
                        reason: "bitwise/concat lowering reached with a non-bitwise operator"
                            .to_string(),
                    })
                }
            };
            // ShiftLeft/ShiftRight/Concat read as `r[P2] <op> r[P1]`
            // (SQLite's own operand order, verified against harvested
            // EXPLAIN) — pass (rhs=P1, lhs=P2) so `lhs <op> rhs` is what's
            // computed. BitAnd/BitOr are commutative, so operand order
            // doesn't change the result.
            match opcode {
                Opcode::ShiftLeft | Opcode::ShiftRight | Opcode::Concat => {
                    em.emit(Instruction::new(opcode, r, l, dest));
                }
                _ => {
                    em.emit(Instruction::new(opcode, l, r, dest));
                }
            }
            Ok(dest)
        }

        // Comparisons and the logical connectives are conditions used
        // in a value context: they answer true/false/unknown, which
        // `compile_bool_to_value` materializes three-valued. Before
        // #134 they fell into the catch-all below and compiled to a
        // bare NULL register, so `SELECT price = 10` answered NULL for
        // every row, NULL operand or not.
        ExprKind::Binary {
            op:
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::And
                | BinaryOp::Or,
            ..
        } => compile_bool_to_value(em, reg, scope, expr),

        // Unreachable in practice: `BinaryOp` has no variant left
        // uncovered by the two arms above (#139). Kept as a defensive
        // fallback rather than a `_ => unreachable!()` so a future
        // `BinaryOp` addition fails soft (wrong answer) instead of
        // panicking mid-query.
        ExprKind::Binary { .. } => {
            let r = reg.alloc();
            em.emit(Instruction::new(Opcode::Null, 0, r, 0));
            Ok(r)
        }

        // #142: `CAST` forces its target affinity via the harvested
        // `Cast` opcode (P2 = the affinity's ASCII byte, matching the
        // oracle's own `EXPLAIN` shape: `Cast r[N], affinity(r[N])`),
        // never `MustBeInt`/`RealAffinity` — those are a guard opcode
        // (aborts instead of truncating, wrong for `CAST('apple' AS
        // INTEGER)` = `0`) and a column-load coercion opcode
        // respectively, neither of which implements `CAST`'s own lossy
        // conversion rule (`src/vdbe/cast.rs`).
        ExprKind::Cast {
            expr: inner,
            type_name,
        } => {
            let r = compile_value(em, reg, scope, inner)?;
            let affinity = affinity_of(type_name);
            let p2 = i32::from(affinity.to_p4_byte());
            em.emit(Instruction::new(Opcode::Cast, r, p2, 0));
            Ok(r)
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
                // A `WHEN` whose condition is unknown is not a match,
                // exactly like a false one — `NullTarget::False`, the
                // same setting `WHERE` uses.
                compile_cond(
                    em,
                    reg,
                    scope,
                    &cond,
                    CondTargets::null_is_false(Target::Fallthrough, Target::Jump(next_label)),
                )?;
                emit_branch_into(em, reg, scope, then_expr, dest)?;
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
                Some(else_expr) => emit_branch_into(em, reg, scope, else_expr, dest)?,
                None => {
                    // This used to fake a NULL with an out-of-range
                    // `Column` read; `Null` (#134) says what it means,
                    // and is what the oracle emits here.
                    em.emit(Instruction::new(Opcode::Null, 0, dest, 0));
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
        | ExprKind::In { .. }
        | ExprKind::Exists { .. }
        | ExprKind::InSubquery { .. }
        | ExprKind::InSubqueryMulti { .. } => compile_bool_to_value(em, reg, scope, expr),

        // #238: a scalar subquery in value position — `SELECT (SELECT
        // max(x) FROM t)`, `x = (SELECT ...)`, etc. #306: if this
        // subquery was hoisted (materialized once, before the enclosing
        // scan's `Rewind`, because it's uncorrelated), its result is
        // already sitting in a register — reuse it instead of
        // re-running the subquery's whole scan on every outer row.
        ExprKind::Subquery(subquery) => {
            let key = crate::codegen::subquery::select_id(subquery);
            match scope.hoisted.get(&key) {
                Some(crate::codegen::subquery::HoistedSubquery::Scalar { reg: r }) => Ok(*r),
                _ => match scope.memoized.get(&key) {
                    // #314: correlated, but memoized per distinct value
                    // of the one outer column it's correlated against.
                    Some(memo) => crate::codegen::subquery::compile_memoized_scalar_subquery(
                        em, reg, scope, subquery, memo,
                    ),
                    None => {
                        crate::codegen::subquery::compile_scalar_subquery(em, reg, scope, subquery)
                    }
                },
            }
        }
    }
}

/// Boolean negation of an already-computed value register (used by
/// `NOT LIKE`/`NOT GLOB`) into a fresh register. The old `IfNot`-based
/// 0/1 materialization resolved a NULL `src` to 1; `Not` propagates it
/// (#134), which is what `x NOT LIKE NULL` has to yield.
fn compile_negate_value(em: &mut Emitter, reg: &mut RegAlloc, src: i32) -> i32 {
    let out = reg.alloc();
    em.emit(Instruction::new(Opcode::Not, src, out, 0));
    out
}

/// CASE's branch results (each computed into its own register) must
/// land in one shared destination. `Literal` and `Column` branches are
/// re-emitted directly into `dest`; any other branch expression is
/// compiled via `compile_value` into its own register and `Copy`'d
/// into `dest` (#141) — evaluating straight into `dest` and leaving it
/// untouched on a re-entrant compile would otherwise risk a stale
/// register from a prior branch or a prior loop iteration leaking out
/// as this branch's result.
fn emit_branch_into(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    scope: &Scope,
    expr: &Expr,
    dest: i32,
) -> Result<(), CodegenError> {
    match &expr.kind {
        ExprKind::Literal(Literal::Integer(i)) => match i32::try_from(*i) {
            Ok(p1) => {
                em.emit(Instruction::new(Opcode::Integer, p1, dest, 0));
            }
            Err(_) => {
                em.emit(Instruction::with_p4(Opcode::Int64, 0, dest, 0, P4::Int(*i)));
            }
        },
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
        ExprKind::Literal(Literal::Float(f)) => {
            em.emit(Instruction::with_p4(Opcode::Real, 0, dest, 0, P4::Real(*f)));
        }
        ExprKind::Literal(Literal::Blob(bytes)) => {
            let len = i32::try_from(bytes.len()).map_err(|_| CodegenError::Unsupported {
                reason: format!(
                    "blob literal of {} bytes does not fit in a P1 operand",
                    bytes.len()
                ),
            })?;
            em.emit(Instruction::with_p4(
                Opcode::Blob,
                len,
                dest,
                0,
                P4::Blob(bytes.clone()),
            ));
        }
        // `dest` is shared across branches and reused every scan
        // iteration, so an explicit NULL branch has to overwrite it.
        // Emitting nothing (the pre-#134 behavior, correct only for a
        // never-written fresh register) leaked the previous row's
        // result out of `SELECT CASE WHEN c THEN x ELSE NULL END`.
        ExprKind::Literal(Literal::Null) => {
            em.emit(Instruction::new(Opcode::Null, 0, dest, 0));
        }
        ExprKind::Column { table, name, .. } => {
            let (cursor, idx, schema, forced_null) = scope.resolve(table.as_deref(), name)?;
            if forced_null {
                em.emit(Instruction::new(Opcode::Null, 0, dest, 0));
            } else {
                emit_column_read(em, schema, cursor, idx, dest)?;
            }
        }
        _ => {
            let r = compile_value(em, reg, scope, expr)?;
            em.emit(Instruction::new(Opcode::Copy, r, dest, 0));
        }
    }
    Ok(())
}

/// Whether a condition's outcome is always definitely true or
/// definitely false — never SQL's unknown. `IS`/`IS NOT` and
/// `IS NULL`/`IS NOT NULL` are the only such conditions in the V2
/// grammar; they exist precisely to answer questions about NULL
/// without inheriting it.
fn is_definite(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Paren(inner) => is_definite(inner),
        // #238: EXISTS is always definitely true or false (see
        // `subquery::compile_exists`'s doc comment) — unlike
        // `InSubquery`, whose NULL-LHS case really is unknown.
        ExprKind::Is { .. } | ExprKind::IsNull { .. } | ExprKind::Exists { .. } => true,
        _ => false,
    }
}

/// Materializes a condition's answer into a register. A condition has
/// three possible answers and jump-mode code only has two
/// destinations, so a genuinely three-valued expression is compiled
/// twice: once asking "is it definitely true?" and once asking "is it
/// definitely false?" (the same condition with `NullTarget::True`, so
/// unknown separates from false instead of joining it). Anything that
/// answers neither is unknown, and lands on the `Null` opcode.
///
/// The alternative — a third continuation threaded through
/// `compile_cond` — does not work: `AND`/`OR` cannot route an unknown
/// left operand anywhere until the right one has been evaluated (see
/// `NullTarget`'s doc comment), so they would have to duplicate their
/// right operand's code anyway, once per path.
fn compile_bool_to_value(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    scope: &Scope,
    expr: &Expr,
) -> Result<i32, CodegenError> {
    let dest = reg.alloc();
    let true_label = em.new_label();
    let end_label = em.new_label();

    if is_definite(expr) {
        compile_cond(
            em,
            reg,
            scope,
            expr,
            CondTargets::null_is_false(Target::Jump(true_label), Target::Fallthrough),
        )?;
        em.emit(Instruction::new(Opcode::Integer, 0, dest, 0));
        em.goto(end_label);
        em.place(true_label);
        em.emit(Instruction::new(Opcode::Integer, 1, dest, 0));
        em.place(end_label);
        return Ok(dest);
    }

    let null_label = em.new_label();
    let false_label = em.new_label();
    // Pass 1: definitely true? Unknown joins false here, so reaching
    // the fallthrough means "false or unknown".
    compile_cond(
        em,
        reg,
        scope,
        expr,
        CondTargets::null_is_false(Target::Jump(true_label), Target::Fallthrough),
    )?;
    // Pass 2: which of the two was it? `NullTarget::True` sends
    // unknown to the true side, which pass 1 already ruled out, so
    // that side can only be reached by an unknown answer.
    compile_cond(
        em,
        reg,
        scope,
        expr,
        CondTargets::null_is_true(Target::Jump(null_label), Target::Jump(false_label)),
    )?;

    em.place(false_label);
    em.emit(Instruction::new(Opcode::Integer, 0, dest, 0));
    em.goto(end_label);
    em.place(null_label);
    em.emit(Instruction::new(Opcode::Null, 0, dest, 0));
    em.goto(end_label);
    em.place(true_label);
    em.emit(Instruction::new(Opcode::Integer, 1, dest, 0));
    em.place(end_label);
    Ok(dest)
}
