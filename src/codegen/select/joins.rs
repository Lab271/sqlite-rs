use super::eqp::table_binding_name;
use super::join_access::{
    choose_join_access, compile_joined_sorted_scan, emit_join_row, resolve_join_order_by,
    JoinAccess,
};
use super::join_full::compile_full_join_two_table;
use super::limit_scan::{compile_limit_setup, emit_limit_guard, emit_offset_guard, LimitState};
use super::order_by::SYNTHETIC_SPAN;
use super::*;
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
///
/// `full_catalog` (#257) is only consulted when a `FROM` slot is a
/// subquery — to resolve *its own* `FROM` table(s), which need not be
/// among `schemas` (the outer query's own joined tables). It's
/// deliberately separate from the `Scope::catalog` a scalar/`IN`/
/// `EXISTS` subquery *expression* inside this JOIN's `WHERE` resolves
/// against (still just `schemas`, unchanged, per the existing scope
/// limitation this function's own doc doesn't relitigate here).
/// `compile_full_join_two_table`'s dedicated path does not accept a
/// subquery-in-FROM (not yet supported there).
pub fn compile_select_joined(
    select: &Select,
    schemas: &[TableSchema],
    full_catalog: &[TableSchema],
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
    if !select.compound.is_empty() {
        return Err(CodegenError::Unsupported {
            reason: "UNION ALL with a JOIN in one of its arms is not yet supported".to_string(),
        });
    }

    // #250's codegen half: `FULL JOIN` gets its own dedicated two-table
    // emitter (see `compile_full_join_two_table`'s doc comment) rather
    // than participating in the `RIGHT`-reordering scheme below — it's
    // only supported as the sole join in the `FROM` clause today. Its
    // pass-1/pass-2 shape doesn't (yet) generalize to ORDER BY/DISTINCT
    // the way the rest of this function's join tree now does, so those
    // stay rejected for a `FULL JOIN` specifically.
    if from.joins.len() == 1 && from.joins.first().is_some_and(|j| j.op == JoinOp::Full) {
        if !select.order_by.is_empty() || matches!(select.distinct, Some(Distinctness::Distinct)) {
            return Err(CodegenError::Unsupported {
                reason: "ORDER BY/DISTINCT combined with a FULL JOIN is not yet supported"
                    .to_string(),
            });
        }
        return compile_full_join_two_table(select, schemas, from);
    }
    if from.joins.iter().any(|j| j.op == JoinOp::Full) {
        return Err(CodegenError::Unsupported {
            reason: "FULL JOIN codegen only supports a single two-table FULL JOIN today \
                     (`SELECT ... FROM a FULL JOIN b ON ...`) — a FULL JOIN combined with \
                     any other join in the same FROM clause is not yet supported"
                .to_string(),
        });
    }
    // RIGHT JOIN is implemented by reordering the join chain into an
    // equivalent LEFT JOIN (`A RIGHT JOIN B` == `B LEFT JOIN A`,
    // generalized to an N-way chain — see the `working_order`/`pos_of`
    // construction below and `LevelPlan`'s doc comment). Only one
    // `RIGHT JOIN` per `FROM` clause is supported: a second one would,
    // in the general case, share its deepest check level with the
    // first (see the design notes accompanying this ticket), which
    // this compiler doesn't attempt to disambiguate — rejected here
    // with a clean error rather than risking a silently wrong plan.
    let right_count = from.joins.iter().filter(|j| j.op == JoinOp::Right).count();
    if right_count > 1 {
        return Err(CodegenError::Unsupported {
            reason: "RIGHT JOIN codegen only supports a single RIGHT JOIN per FROM clause \
                     today — a chain with more than one RIGHT JOIN is not yet supported"
                .to_string(),
        });
    }

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();

    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    let end_label = em.new_label();
    let mut sink = |em: &mut Emitter, _reg: &mut RegAlloc, first: i32, count: i32| {
        em.emit(Instruction::new(Opcode::ResultRow, first, count, 0));
        Ok(())
    };
    compile_select_joined_scan(
        &mut em,
        &mut reg,
        select,
        schemas,
        full_catalog,
        0,
        end_label,
        &mut sink,
    )?;

    em.place(end_label);
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

    Ok(em.finish())
}

/// The scan/filter/project core of [`compile_select_joined`] (INNER/LEFT/
/// CROSS/NATURAL/USING/RIGHT — `FULL JOIN` stays on its own dedicated
/// [`compile_full_join_two_table`] path), minus the `Init`/`Halt`
/// bracketing and with every cursor number offset by `cursor_base` —
/// factored out, mirroring [`compile_select_scan`]'s relationship to
/// [`compile_select`], so #250's `INSERT ... SELECT` codegen can drive
/// the same joined nested-loop scan with its own cursor numbers already
/// claimed by the target table/its indexes, substituting its own row
/// sink in place of `ResultRow`.
///
/// `ORDER BY` and `DISTINCT` are both supported here now (#250's last
/// piece): `ORDER BY` routes through a dedicated sort pass1/pass2 (see
/// [`compile_join_level_for_sort`]) whose pass-2 result-column
/// reconstruction is restricted to `*`/`table.*`/bare-column result
/// columns (a computed expression in the `SELECT` list combined with a
/// joined `ORDER BY` returns a clean `Unsupported` error rather than
/// silently mis-projecting); `DISTINCT` (without `ORDER BY`) instead
/// hooks directly into the ordinary nested-loop scan's final-row
/// emission via an ephemeral-index guard, since it never needs the
/// sorter at all. The two combined on a JOIN are rejected outright — see
/// the caller.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_select_joined_scan<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    schemas: &[TableSchema],
    full_catalog: &[TableSchema],
    cursor_base: i32,
    end_label: Label,
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    let Some(from) = &select.from else {
        return Err(CodegenError::NoFromClause);
    };
    if !select.order_by.is_empty() && matches!(select.distinct, Some(Distinctness::Distinct)) {
        return Err(CodegenError::Unsupported {
            reason: "DISTINCT combined with ORDER BY and a JOIN is not yet supported".to_string(),
        });
    }
    // `FULL JOIN` has its own dedicated two-table emitter
    // (`compile_full_join_two_table`), reached only through
    // `compile_select_joined`'s own dispatch above this function — a
    // caller reaching `compile_select_joined_scan` directly (#250's
    // `INSERT ... SELECT`, see `insert.rs::compile_insert`) never goes
    // through that dispatch, so a `FULL JOIN` in `select.from.joins`
    // here would otherwise silently be treated like an ordinary
    // inner/left join by the nested-loop machinery below. Rejected
    // explicitly instead.
    if from.joins.iter().any(|j| j.op == JoinOp::Full) {
        return Err(CodegenError::Unsupported {
            reason: "FULL JOIN combined with INSERT ... SELECT is not yet supported".to_string(),
        });
    }
    let right_count = from.joins.iter().filter(|j| j.op == JoinOp::Right).count();
    if right_count > 1 {
        return Err(CodegenError::Unsupported {
            reason: "RIGHT JOIN codegen only supports a single RIGHT JOIN per FROM clause \
                     today — a chain with more than one RIGHT JOIN is not yet supported"
                .to_string(),
        });
    }

    let table_refs: Vec<&TableRef> = std::iter::once(&from.first)
        .chain(from.joins.iter().map(|j| &j.table))
        .collect();
    let n = schemas.len();
    // `bindings` stays in original FROM-clause order throughout — its
    // `cursor` field is filled in below once the execution order
    // (`working_order`) is known, and it (not the reordered execution
    // list) is what every `Scope` gets built from, so `SELECT *`
    // expansion order and column-ambiguity resolution are unaffected
    // by RIGHT JOIN's internal reordering.
    let mut bindings = Vec::with_capacity(n);
    for (table_ref, schema) in table_refs.iter().zip(schemas.iter()) {
        bindings.push(TableBinding {
            alias: table_ref.alias.clone(),
            name: table_binding_name(table_ref),
            schema: schema.clone(),
            cursor: 0,
            forced_null: false,
        });
    }

    // `dedup_star[i]` names the columns (lowercased) that a plain `*`
    // expansion must skip for `bindings[i]` — populated below for the
    // *right*-hand side of each NATURAL/USING join (#250's codegen
    // half), since SQLite keeps only the left-most occurrence of a
    // naturally-/USING-joined column in `SELECT *` output. Indexed by
    // *original* FROM-clause position, same as `bindings`.
    let mut dedup_star: Vec<std::collections::HashSet<String>> =
        vec![std::collections::HashSet::new(); n];
    let mut constraints: Vec<Option<Expr>> = Vec::with_capacity(from.joins.len());
    for (i, join) in from.joins.iter().enumerate() {
        let right_idx = i.checked_add(1).ok_or_else(|| CodegenError::Unsupported {
            reason: "too many joined tables".to_string(),
        })?;
        let left = bindings
            .get(0..right_idx)
            .ok_or_else(|| CodegenError::Unsupported {
                reason: "join level out of range".to_string(),
            })?;
        let right = bindings
            .get(right_idx)
            .ok_or_else(|| CodegenError::Unsupported {
                reason: "join level out of range".to_string(),
            })?;
        let constraint = match &join.constraint {
            Some(JoinConstraint::On(e)) => Some(e.clone()),
            Some(JoinConstraint::Using(cols)) => {
                let (expr, shared) = synthesize_equality_constraint(left, right, cols, true)?;
                if let Some(slot) = dedup_star.get_mut(right_idx) {
                    slot.extend(shared);
                }
                expr
            }
            None if join.natural => {
                let shared_names: Vec<String> = right
                    .schema
                    .columns
                    .iter()
                    .filter(|name| {
                        left.iter().any(|b| {
                            b.schema
                                .columns
                                .iter()
                                .any(|c| c.eq_ignore_ascii_case(name))
                        })
                    })
                    .cloned()
                    .collect();
                if shared_names.is_empty() {
                    None
                } else {
                    let (expr, shared) =
                        synthesize_equality_constraint(left, right, &shared_names, false)?;
                    if let Some(slot) = dedup_star.get_mut(right_idx) {
                        slot.extend(shared);
                    }
                    expr
                }
            }
            None => None,
        };
        constraints.push(constraint);
    }

    // Determine execution order: `working_order[exec_pos]` is the
    // original FROM-clause index executed at that recursion level.
    // Every `Inner`/`Left`/`Cross` join (including already-resolved
    // NATURAL/USING) simply appends its table to the end, exactly like
    // #237. A `Right` join instead *prepends* its table to the front —
    // `A RIGHT JOIN B` becomes `B`'s cursor loop outermost, with the
    // entire prior chain (everything already in `working_order`)
    // nested beneath it as the side that gets null-extended on a miss,
    // i.e. exactly `B LEFT JOIN A`. `right_count <= 1` is enforced
    // above, so at most one such prepend ever happens.
    struct NormalStep {
        table: usize,
        is_left: bool,
        join_index: usize,
    }
    struct RightStep {
        new_table: usize,
        deep_orig: usize,
        join_index: usize,
    }
    let mut working_order: Vec<usize> = vec![0];
    let mut normal_steps: Vec<NormalStep> = Vec::with_capacity(from.joins.len());
    let mut right_step: Option<RightStep> = None;
    for (j, join) in from.joins.iter().enumerate() {
        let new_table = j.saturating_add(1);
        if join.op == JoinOp::Right {
            let deep_orig = *working_order.last().unwrap_or(&0);
            right_step = Some(RightStep {
                new_table,
                deep_orig,
                join_index: j,
            });
            working_order = std::iter::once(new_table)
                .chain(working_order.iter().copied())
                .collect();
        } else {
            normal_steps.push(NormalStep {
                table: new_table,
                is_left: join.op == JoinOp::Left,
                join_index: j,
            });
            working_order.push(new_table);
        }
    }

    // `pos_of[original_index]` is the execution-order recursion level
    // that original table ends up at.
    let mut pos_of = vec![0usize; n];
    for (pos, &orig) in working_order.iter().enumerate() {
        if let Some(slot) = pos_of.get_mut(orig) {
            *slot = pos;
        }
    }

    // Cursor numbers follow execution order (simplest: a table's
    // cursor number is just its recursion level), and every cursor is
    // opened exactly once, in that same order — `OpenRead` for a real
    // table, or (#257) materialized into an ephemeral table when this
    // FROM slot is a subquery.
    for (pos, &orig) in working_order.iter().enumerate() {
        let cursor = cursor_base.saturating_add(i32::try_from(pos).unwrap_or(0));
        if let Some(binding) = bindings.get_mut(orig) {
            binding.cursor = cursor;
        }
        match table_refs.get(orig).map(|t| &t.kind) {
            Some(TableRefKind::Subquery(subquery)) => {
                crate::codegen::subquery::materialize_from_subquery(
                    em,
                    reg,
                    subquery,
                    full_catalog,
                    cursor,
                )?;
            }
            _ => {
                let root_page = bindings
                    .get(orig)
                    .map(|b| i32::try_from(b.schema.root_page).unwrap_or(0))
                    .unwrap_or(0);
                em.emit(Instruction::new(Opcode::OpenRead, cursor, root_page, 0));
            }
        }
    }

    let exec_bindings: Vec<TableBinding> = working_order
        .iter()
        .filter_map(|&orig| bindings.get(orig).cloned())
        .collect();

    // Per-execution-level plan: `levels[level]` describes what to check
    // while iterating `exec_bindings[level]`'s own loop, and whether
    // this level owns an outer-join "matched" register.
    let mut levels: Vec<LevelPlan> = vec![LevelPlan::default(); n];
    for step in &normal_steps {
        let pos = pos_of.get(step.table).copied().unwrap_or(0);
        let constraint = constraints.get(step.join_index).cloned().flatten();
        if let Some(plan) = levels.get_mut(pos) {
            plan.checks.push(LevelCheck {
                constraint,
                sets_matched: if step.is_left { Some(pos) } else { None },
            });
            if step.is_left {
                plan.null_span = Some((pos, pos));
            }
        }
    }
    if let Some(rs) = &right_step {
        // `rs.new_table`'s own execution level (`outer_pos`) needs no
        // special handling at all — it's a plain unconditional scan,
        // exactly as if it were `from.first` (nothing shallower depends
        // on it). The outer-join bookkeeping (matched register,
        // null-extension) belongs to its *immediate child* level
        // (`outer_pos + 1`) instead — reset before that level's own
        // `Rewind`, checked after its own loop exhausts, precisely
        // mirroring a classic `LEFT JOIN`'s placement (whose "matched"
        // owner is likewise the LEFT-joined table's own level, nested
        // inside its parent's loop for the right per-row cadence) —
        // only here `check_pos` (where the constraint actually gets
        // evaluated) may be deeper than the owning level whenever the
        // pre-existing chain being RIGHT-joined against has more than
        // one table.
        let outer_pos = pos_of.get(rs.new_table).copied().unwrap_or(0);
        let check_pos = pos_of.get(rs.deep_orig).copied().unwrap_or(0);
        let owner_pos = outer_pos.saturating_add(1);
        let constraint = constraints.get(rs.join_index).cloned().flatten();
        if let Some(plan) = levels.get_mut(check_pos) {
            plan.checks.push(LevelCheck {
                constraint,
                sets_matched: Some(owner_pos),
            });
        }
        if let Some(plan) = levels.get_mut(owner_pos) {
            plan.null_span = Some((owner_pos, check_pos));
        }
    }

    let full_scope = Scope {
        tables: bindings.clone(),
        catalog: schemas.to_vec(),
        outer: None,
        dedup_star: dedup_star.clone(),
    };
    let table_cursor_count = i32::try_from(n).unwrap_or(0);

    if !select.order_by.is_empty() {
        let order_by_plans = resolve_join_order_by(select, &full_scope)?;
        let sort_cursor = cursor_base.saturating_add(table_cursor_count);
        let pseudo_cursor = sort_cursor.saturating_add(1);
        return compile_joined_sorted_scan(
            em,
            reg,
            select,
            &exec_bindings,
            &bindings,
            &pos_of,
            &levels,
            &dedup_star,
            schemas,
            &full_scope,
            &order_by_plans,
            sort_cursor,
            pseudo_cursor,
            end_label,
            sink,
        );
    }

    let limit = compile_limit_setup(em, reg, &full_scope, select)?;
    let distinct_cursor = matches!(select.distinct, Some(Distinctness::Distinct)).then(|| {
        let cursor = cursor_base.saturating_add(table_cursor_count);
        em.emit(Instruction::new(Opcode::OpenEphemeral, cursor, 0, 0));
        cursor
    });

    let mut null_mask = vec![false; n];
    let mut matched_regs: Vec<Option<i32>> = vec![None; n];
    compile_join_level(
        em,
        reg,
        select,
        &exec_bindings,
        &bindings,
        &pos_of,
        &levels,
        &dedup_star,
        &mut null_mask,
        &mut matched_regs,
        0,
        end_label,
        limit.as_ref(),
        distinct_cursor,
        schemas,
        sink,
    )
}

/// Builds the [`Scope`] a join-tree node sees at compile time. `bindings`
/// is always in *original* FROM-clause order (so `SELECT *` expansion
/// order and column-ambiguity resolution never depend on RIGHT JOIN's
/// internal execution reordering — see [`compile_select_joined`]);
/// `null_mask` is indexed by *execution* level instead, so `pos_of`
/// (original index -> execution level) translates between the two:
/// binding `orig` is forced null when `null_mask[pos_of[orig]]` is set
/// (an outer join's no-match branch, see [`compile_join_level`]) — the
/// shared `bindings` vec itself is never mutated.
pub(super) fn join_scope(
    bindings: &[TableBinding],
    null_mask: &[bool],
    pos_of: &[usize],
    catalog: &[TableSchema],
    dedup_star: &[std::collections::HashSet<String>],
) -> Scope {
    Scope {
        tables: bindings
            .iter()
            .enumerate()
            .map(|(orig, b)| {
                let forced_null = pos_of
                    .get(orig)
                    .and_then(|&pos| null_mask.get(pos))
                    .copied()
                    .unwrap_or(false)
                    || b.forced_null;
                TableBinding {
                    alias: b.alias.clone(),
                    name: b.name.clone(),
                    schema: b.schema.clone(),
                    cursor: b.cursor,
                    forced_null,
                }
            })
            .collect(),
        catalog: catalog.to_vec(),
        outer: None,
        dedup_star: dedup_star.to_vec(),
    }
}

/// Builds the qualified-column `Expr` used to reference `binding`'s
/// `name` column when synthesizing a NATURAL/USING join's equality
/// constraint — qualified (rather than a bare unqualified `Column`) so
/// resolution never has to fall back to [`Scope::resolve`]'s
/// unqualified-ambiguity rule, which would incorrectly reject a column
/// name shared by more than one already-joined left-side table.
pub(super) fn qualified_column_expr(binding: &TableBinding, name: &str) -> Expr {
    Expr {
        kind: ExprKind::Column {
            table: Some(
                binding
                    .alias
                    .clone()
                    .unwrap_or_else(|| binding.name.clone()),
            ),
            catalog: None,
            name: name.to_string(),
        },
        span: SYNTHETIC_SPAN,
    }
}

/// Synthesizes the `ON`-equivalent equality constraint for a NATURAL
/// or `USING (...)` join: for each name in `cols`, finds a left-side
/// binding (searched in `left`, i.e. `bindings[0..=i]`, first match
/// wins — this is the "simplest defensible interpretation" for 3+-way
/// chains noted in #250's follow-up plan, since a qualified reference
/// to that one binding's column sidesteps the unqualified-ambiguity
/// question entirely) and requires `right` (`bindings[i + 1]`) to have
/// a same-named column, ANDing `left.col = right.col` together across
/// every name. Returns the synthesized `Expr` (`None` only if `cols`
/// is empty) plus the exact schema-cased column names used, so the
/// caller can also populate `dedup_star` for `SELECT *`
/// de-duplication.
pub(super) fn synthesize_equality_constraint(
    left: &[TableBinding],
    right: &TableBinding,
    cols: &[String],
    require_left_match: bool,
) -> Result<(Option<Expr>, Vec<String>), CodegenError> {
    let mut acc: Option<Expr> = None;
    let mut shared = Vec::with_capacity(cols.len());
    for name in cols {
        let left_binding = left.iter().find(|b| {
            b.schema
                .columns
                .iter()
                .any(|c| c.eq_ignore_ascii_case(name))
        });
        let Some(left_binding) = left_binding else {
            if require_left_match {
                return Err(CodegenError::UnknownColumn { name: name.clone() });
            }
            continue;
        };
        let right_idx = column_index(&right.schema, name)
            .ok_or_else(|| CodegenError::UnknownColumn { name: name.clone() })?;
        let right_name = right
            .schema
            .columns
            .get(right_idx)
            .cloned()
            .ok_or_else(|| CodegenError::UnknownColumn { name: name.clone() })?;
        let eq = Expr {
            kind: ExprKind::Binary {
                op: BinaryOp::Eq,
                lhs: Box::new(qualified_column_expr(left_binding, name)),
                rhs: Box::new(qualified_column_expr(right, &right_name)),
            },
            span: SYNTHETIC_SPAN,
        };
        acc = Some(match acc {
            Some(prev) => Expr {
                kind: ExprKind::Binary {
                    op: BinaryOp::And,
                    lhs: Box::new(prev),
                    rhs: Box::new(eq),
                },
                span: SYNTHETIC_SPAN,
            },
            None => eq,
        });
        shared.push(name.to_ascii_lowercase());
    }
    Ok((acc, shared))
}

/// One constraint checked while iterating `exec_bindings[check_level]`'s
/// own loop (see [`compile_join_level`]): `constraint` gates whether
/// recursion continues to the next level (`None` means unconditional —
/// a `CROSS`/`NATURAL`-with-no-shared-columns join), and if
/// `sets_matched` is `Some(outer_level)`, passing it also marks
/// `outer_level`'s "matched" register. For a classic `LEFT JOIN`,
/// `outer_level == check_level` (the table's own loop both checks its
/// `ON` condition and owns the matched flag, exactly #237's original
/// shape). For `RIGHT JOIN` reordered into an equivalent `LEFT JOIN`
/// (see [`compile_select_joined`]), `outer_level` is the RIGHT-joined
/// table's own (shallower) level, while `check_level` is the deepest
/// level of the chain it was joined against — the constraint can only
/// be evaluated once every table it references is bound.
#[derive(Debug, Clone)]
pub(super) struct LevelCheck {
    pub(super) constraint: Option<Expr>,
    pub(super) sets_matched: Option<usize>,
}

/// The full plan for one execution level: zero or more [`LevelCheck`]s
/// run inside its own `Rewind`/`Next` loop, and — if `null_span` is
/// `Some((start, end))` — this level owns an outer-join "matched"
/// register, tested once its own loop exhausts. If nothing matched,
/// every level in `start..=end` (inclusive, always this level or
/// deeper) gets `null_mask` forced on and recursion jumps directly to
/// `end + 1`, skipping those levels' own loops entirely — there is
/// nothing to iterate for a synthesized outer-join row. A classic
/// `LEFT JOIN` has `null_span == Some((level, level))` (only itself);
/// `RIGHT JOIN`'s reordering produces `null_span == Some((outer_level +
/// 1, check_level))`, spanning every level of the chain it was joined
/// against.
#[derive(Debug, Clone, Default)]
pub(super) struct LevelPlan {
    pub(super) checks: Vec<LevelCheck>,
    pub(super) null_span: Option<(usize, usize)>,
}

/// Recursively emits the nested-loop join, one table per recursion
/// level. `exec_bindings` is in *execution* order (level `i` opens
/// `exec_bindings[i]`'s cursor); `orig_bindings`/`pos_of` are the
/// original FROM-clause-order bindings and the original-index ->
/// execution-level map, used only to build a [`Scope`] in FROM order
/// (see [`join_scope`]) — `SELECT *` expansion and column-ambiguity
/// resolution must not depend on RIGHT JOIN's internal reordering.
/// `level == exec_bindings.len()` is the innermost point — every
/// table's cursor is positioned on a candidate combination, so this is
/// where `WHERE`, `LIMIT`/`OFFSET`, and the result-column projection
/// all compile, via [`emit_join_final_row`].
///
/// A level with `levels[level].null_span == Some((start, end))` wraps
/// its own `Rewind`/`Next` loop with a `matched` flag register:
/// cleared before the loop, set to 1 by any [`LevelCheck`] (at this
/// level or a deeper `check_level`) whose `sets_matched` names this
/// level, and tested with `IfNot` right after the loop exits — if it's
/// still 0, the join recurses exactly once more (jumping straight to
/// `end + 1`) with `null_mask` set for every level in `start..=end`,
/// which (per [`join_scope`]) makes every reference to those tables'
/// columns compile to a NULL literal instead of a real `Column`/
/// `Rowid` read, so a non-matching row still contributes exactly one
/// null-extended output row.
#[allow(clippy::too_many_arguments)]
pub(super) fn compile_join_level<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    exec_bindings: &[TableBinding],
    orig_bindings: &[TableBinding],
    pos_of: &[usize],
    levels: &[LevelPlan],
    dedup_star: &[std::collections::HashSet<String>],
    null_mask: &mut Vec<bool>,
    matched_regs: &mut Vec<Option<i32>>,
    level: usize,
    end_label: Label,
    limit: Option<&LimitState>,
    distinct_cursor: Option<i32>,
    catalog: &[TableSchema],
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    compile_join_level_traverse(
        em,
        reg,
        exec_bindings,
        orig_bindings,
        pos_of,
        levels,
        dedup_star,
        null_mask,
        matched_regs,
        level,
        catalog,
        &mut |em, reg, scope| {
            emit_join_final_row(em, reg, select, scope, end_label, limit, distinct_cursor, sink)
        },
    )
}

/// Shared nested-loop/outer-join traversal behind both [`compile_join_level`]
/// (the unsorted path) and [`super::join_access::compile_join_level_for_sort`]
/// (#250's `ORDER BY`+JOIN sorted path) — every level's `Rewind`/`Next` loop,
/// `ON`-condition checks, `#243` single-check-access seek optimization, and
/// `LEFT`/`RIGHT` "matched"-register/null-extension bookkeeping lives here
/// exactly once, so the sorted path can no longer silently miss the seek
/// optimization the unsorted path gets (the bug this extraction fixes: the
/// two paths used to be hand-forked copies, and only one of them ever grew
/// the #243 seek). `leaf` is invoked once per candidate combination (i.e.
/// once `level == exec_bindings.len()`) with the fully-resolved [`Scope`];
/// each caller supplies its own innermost emission (direct row emit for the
/// unsorted path, sort-buffer write for the sorted path) via `leaf`.
#[allow(clippy::too_many_arguments)]
pub(super) fn compile_join_level_traverse<L>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    exec_bindings: &[TableBinding],
    orig_bindings: &[TableBinding],
    pos_of: &[usize],
    levels: &[LevelPlan],
    dedup_star: &[std::collections::HashSet<String>],
    null_mask: &mut Vec<bool>,
    matched_regs: &mut Vec<Option<i32>>,
    level: usize,
    catalog: &[TableSchema],
    leaf: &mut L,
) -> Result<(), CodegenError>
where
    L: FnMut(&mut Emitter, &mut RegAlloc, &Scope) -> Result<(), CodegenError>,
{
    if level == exec_bindings.len() {
        let scope = join_scope(orig_bindings, null_mask, pos_of, catalog, dedup_star);
        return leaf(em, reg, &scope);
    }

    let Some(binding) = exec_bindings.get(level) else {
        return Err(CodegenError::Unsupported {
            reason: "join level out of range".to_string(),
        });
    };
    let cursor = binding.cursor;
    let plan = levels.get(level).cloned().unwrap_or_default();

    if plan.null_span.is_some() {
        let matched = reg.alloc();
        em.emit(Instruction::new(Opcode::Integer, 0, matched, 0));
        if let Some(slot) = matched_regs.get_mut(level) {
            *slot = Some(matched);
        }
    }

    // #243's SeekRowid/SeekIndexEq point-lookup optimization, adapted to
    // this reordering-aware plan: only tried when the level has exactly
    // one check (the common case — a RIGHT JOIN group's deepest level
    // combining an intra-group condition with the outer join's own
    // condition has two, and always falls back to the full scan below;
    // see [`choose_join_access`]'s own narrowness for the rest of the
    // conditions this applies under).
    let single_check_access = match plan.checks.as_slice() {
        [check] => check.constraint.as_ref().and_then(|constraint| {
            let prior = exec_bindings.get(..level)?;
            choose_join_access(binding, constraint, prior)
        }),
        _ => None,
    };

    match single_check_access {
        Some(access) => {
            let scope = join_scope(orig_bindings, null_mask, pos_of, catalog, dedup_star);
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
                    let index_cursor = i32::try_from(exec_bindings.len().saturating_add(level))
                        .unwrap_or(i32::MAX);
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
            if let Some(outer_level) = plan.checks.first().and_then(|c| c.sets_matched) {
                let target = matched_regs
                    .get(outer_level)
                    .copied()
                    .flatten()
                    .ok_or_else(|| CodegenError::Unsupported {
                        reason: "join level plan referenced an unallocated matched register"
                            .to_string(),
                    })?;
                em.emit(Instruction::new(Opcode::Integer, 1, target, 0));
            }
            let next_level = level.saturating_add(1);
            compile_join_level_traverse(
                em,
                reg,
                exec_bindings,
                orig_bindings,
                pos_of,
                levels,
                dedup_star,
                null_mask,
                matched_regs,
                next_level,
                catalog,
                leaf,
            )?;
            em.place(miss);
        }
        None => {
            let rewind_end = em.new_label();
            let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, cursor, 0, 0));
            em.patch_p2(rewind_addr, rewind_end);
            let loop_start = em.new_label();
            em.place(loop_start);

            let skip = em.new_label();
            for check in &plan.checks {
                if let Some(constraint) = &check.constraint {
                    let scope = join_scope(orig_bindings, null_mask, pos_of, catalog, dedup_star);
                    compile_cond(
                        em,
                        reg,
                        &scope,
                        constraint,
                        CondTargets::null_is_false(Target::Fallthrough, Target::Jump(skip)),
                    )?;
                }
                if let Some(outer_level) = check.sets_matched {
                    let target = matched_regs
                        .get(outer_level)
                        .copied()
                        .flatten()
                        .ok_or_else(|| CodegenError::Unsupported {
                            reason: "join level plan referenced an unallocated matched register"
                                .to_string(),
                        })?;
                    em.emit(Instruction::new(Opcode::Integer, 1, target, 0));
                }
            }
            let next_level = level.saturating_add(1);
            compile_join_level_traverse(
                em,
                reg,
                exec_bindings,
                orig_bindings,
                pos_of,
                levels,
                dedup_star,
                null_mask,
                matched_regs,
                next_level,
                catalog,
                leaf,
            )?;
            em.place(skip);
            let next_addr = em.emit(Instruction::new(Opcode::Next, cursor, 0, 0));
            em.patch_p2(next_addr, loop_start);
            em.place(rewind_end);
        }
    }

    if let Some((start, end)) = plan.null_span {
        let matched = matched_regs.get(level).copied().flatten().ok_or_else(|| {
            CodegenError::Unsupported {
                reason: "join level plan missing matched register for outer join".to_string(),
            }
        })?;
        // `matched` is still 0 iff nothing satisfied this outer join —
        // emit exactly one null-extended row for `start..=end` in that
        // case, then continue from `end + 1` (skipping those levels'
        // own loops entirely — there's nothing to iterate).
        let do_null = em.new_label();
        let after_null = em.new_label();
        let addr = em.emit(Instruction::new(Opcode::IfNot, matched, 0, 0));
        em.patch_p2(addr, do_null);
        em.goto(after_null);

        em.place(do_null);
        for lv in start..=end {
            if let Some(slot) = null_mask.get_mut(lv) {
                *slot = true;
            }
        }
        compile_join_level_traverse(
            em,
            reg,
            exec_bindings,
            orig_bindings,
            pos_of,
            levels,
            dedup_star,
            null_mask,
            matched_regs,
            end.saturating_add(1),
            catalog,
            leaf,
        )?;
        for lv in start..=end {
            if let Some(slot) = null_mask.get_mut(lv) {
                *slot = false;
            }
        }
        em.place(after_null);
    }
    Ok(())
}

/// Applies `WHERE`, `LIMIT`/`OFFSET`, and the result-column projection
/// to one candidate join row (`scope` already reflects every table's
/// forced-null state for this branch) — factored out of
/// [`compile_join_level`]'s innermost level so [`compile_full_join_two_table`]
/// can reuse the exact same sequencing for its own three emission
/// points (matched, left-nulled, right-unmatched).
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_join_final_row<F>(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    scope: &Scope,
    end_label: Label,
    limit: Option<&LimitState>,
    distinct_cursor: Option<i32>,
    sink: &mut F,
) -> Result<(), CodegenError>
where
    F: FnMut(&mut Emitter, &mut RegAlloc, i32, i32) -> Result<(), CodegenError>,
{
    let row_skip = em.new_label();
    if let Some(where_expr) = &select.where_clause {
        compile_cond(
            em,
            reg,
            scope,
            where_expr,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(row_skip)),
        )?;
    }
    if let Some(distinct_cursor) = distinct_cursor {
        emit_join_distinct_guard(em, reg, select, scope, distinct_cursor, row_skip)?;
    }
    if let Some(limit) = limit {
        emit_offset_guard(em, limit, row_skip);
    }
    if let Some(limit) = limit {
        emit_limit_guard(em, limit, end_label);
    }
    emit_join_row(em, reg, select, scope, sink)?;
    em.place(row_skip);
    Ok(())
}

/// #250: `DISTINCT` combined with a JOIN. Same ephemeral-index dedup
/// mechanism as the single-table [`emit_distinct_guard`] — `Found`
/// against `distinct_cursor` skips an already-seen row, `IdxInsert`
/// records a new one — but keyed by `select`'s result columns projected
/// against the joined `scope` via [`emit_join_row`] rather than a single
/// schema. The projection is computed twice (once here to test/record
/// it, once more via the ordinary [`emit_join_row`] call right after in
/// [`emit_join_final_row`]) rather than threading the registers through
/// — both computations are side-effect-free column reads/literal
/// expressions, so the only cost is a handful of extra bump-allocated
/// registers, which this compiler already treats as cheap.
pub(super) fn emit_join_distinct_guard(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    select: &Select,
    scope: &Scope,
    distinct_cursor: i32,
    skip_label: Label,
) -> Result<(), CodegenError> {
    let mut captured: Option<(i32, i32)> = None;
    emit_join_row(em, reg, select, scope, &mut |_em, _reg, first, count| {
        captured = Some((first, count));
        Ok(())
    })?;
    let Some((first, count)) = captured else {
        return Ok(());
    };
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
