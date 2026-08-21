use super::eqp::table_binding_name;
use super::joins::{emit_join_final_row, resolve_join_constraint};
use super::limit_scan::compile_limit_setup;
use super::*;
/// #250: `A FULL JOIN B ON cond` (or `USING (...)`/`NATURAL`),
/// restricted to the two-table case — `compile_select_joined` only
/// calls this when `FULL` is the sole join in the `FROM` clause; any
/// other shape (a `FULL JOIN` combined with another join) is rejected
/// there with a clean `Unsupported` error instead.
///
/// `A FULL JOIN B ON cond` is exactly `(A LEFT JOIN B ON cond)` rows,
/// plus any row of `B` matched by no row of `A` at all (null-extended
/// on `A`'s side instead). This is *not* simply `A LEFT JOIN B` unioned
/// with `B LEFT JOIN A` — that would double-count every matched pair —
/// so pass 1 runs the ordinary two-table LEFT JOIN nested loop
/// (mirroring the shape [`compile_join_level`] emits for a plain LEFT
/// JOIN), additionally recording every matched `B` rowid into an
/// ephemeral index the moment `cond` passes (mirroring
/// `emit_distinct_guard`'s `OpenEphemeral`/`Found`/`IdxInsert` dedup
/// mechanism, keyed by `B`'s rowid instead of a result-row tuple); pass
/// 2 then re-scans `B` and emits one `A`-nulled row for every `B`
/// rowid pass 1 never recorded. `WHERE`/`LIMIT`/`OFFSET` apply
/// identically at all three emission points via
/// [`emit_join_final_row`].
pub(super) fn compile_full_join_two_table(
    select: &Select,
    schemas: &[TableSchema],
    from: &FromClause,
) -> Result<Program, CodegenError> {
    let table_refs: Vec<&TableRef> = std::iter::once(&from.first)
        .chain(from.joins.iter().map(|j| &j.table))
        .collect();

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();
    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    let mut bindings = Vec::with_capacity(2);
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
            name: table_binding_name(table_ref),
            schema: schema.clone(),
            cursor,
            forced_null: false,
        });
    }
    let Some(join) = from.joins.first() else {
        return Err(CodegenError::Unsupported {
            reason: "FULL JOIN codegen only supports a single two-table FULL JOIN today"
                .to_string(),
        });
    };
    let out_of_range = || CodegenError::Unsupported {
        reason: "FULL JOIN codegen only supports a single two-table FULL JOIN today".to_string(),
    };
    let binding_a = bindings.first().cloned().ok_or_else(out_of_range)?;
    let binding_b = bindings.get(1).cloned().ok_or_else(out_of_range)?;

    let mut dedup_star: Vec<std::collections::HashSet<String>> =
        vec![std::collections::HashSet::new(); 2];
    let left = std::slice::from_ref(&binding_a);
    let constraint = resolve_join_constraint(join, left, &binding_b, 1, &mut dedup_star)?;

    let full_scope = Scope {
        tables: bindings.clone(),
        catalog: schemas.to_vec(),
        outer: None,
        dedup_star: dedup_star.clone(),
    };
    let limit = compile_limit_setup(&mut em, &mut reg, &full_scope, select)?;

    // Ephemeral index tracking every `B` rowid matched during pass 1 —
    // same mechanism `emit_distinct_guard` uses for DISTINCT, keyed by
    // `B`'s rowid instead of a result-row tuple.
    let eph_cursor: i32 = 2;
    em.emit(Instruction::new(Opcode::OpenEphemeral, eph_cursor, 0, 0));

    let end_label = em.new_label();
    let mut sink = |em: &mut Emitter, _reg: &mut RegAlloc, first: i32, count: i32| {
        em.emit(Instruction::new(Opcode::ResultRow, first, count, 0));
        Ok(())
    };

    let a_cursor = binding_a.cursor;
    let b_cursor = binding_b.cursor;
    let matched = reg.alloc();

    // Pass 1: `A LEFT JOIN B ON cond`, instrumented to record every
    // matched `B` rowid.
    let a_rewind_end = em.new_label();
    let a_rewind = em.emit(Instruction::new(Opcode::Rewind, a_cursor, 0, 0));
    em.patch_p2(a_rewind, a_rewind_end);
    let a_loop = em.new_label();
    em.place(a_loop);

    em.emit(Instruction::new(Opcode::Integer, 0, matched, 0));

    let b_rewind_end = em.new_label();
    let b_rewind = em.emit(Instruction::new(Opcode::Rewind, b_cursor, 0, 0));
    em.patch_p2(b_rewind, b_rewind_end);
    let b_loop = em.new_label();
    em.place(b_loop);

    let b_skip = em.new_label();
    let match_scope = Scope {
        tables: bindings.clone(),
        catalog: schemas.to_vec(),
        outer: None,
        dedup_star: dedup_star.clone(),
    };
    if let Some(c) = &constraint {
        compile_cond(
            &mut em,
            &mut reg,
            &match_scope,
            c,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(b_skip)),
        )?;
    }
    em.emit(Instruction::new(Opcode::Integer, 1, matched, 0));
    let rowid_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Rowid, b_cursor, rowid_reg, 0));
    em.emit(Instruction::with_p4(
        Opcode::IdxInsert,
        eph_cursor,
        rowid_reg,
        0,
        P4::Int(1),
    ));
    emit_join_final_row(
        &mut em,
        &mut reg,
        select,
        &match_scope,
        end_label,
        limit.as_ref(),
        None,
        &mut sink,
    )?;
    em.place(b_skip);
    let b_next = em.emit(Instruction::new(Opcode::Next, b_cursor, 0, 0));
    em.patch_p2(b_next, b_loop);
    em.place(b_rewind_end);

    let do_null = em.new_label();
    let after_null = em.new_label();
    let addr = em.emit(Instruction::new(Opcode::IfNot, matched, 0, 0));
    em.patch_p2(addr, do_null);
    em.goto(after_null);
    em.place(do_null);
    let mut b_null_bindings = bindings.clone();
    if let Some(b) = b_null_bindings.get_mut(1) {
        b.forced_null = true;
    }
    let b_null_scope = Scope {
        tables: b_null_bindings,
        catalog: schemas.to_vec(),
        outer: None,
        dedup_star: dedup_star.clone(),
    };
    emit_join_final_row(
        &mut em,
        &mut reg,
        select,
        &b_null_scope,
        end_label,
        limit.as_ref(),
        None,
        &mut sink,
    )?;
    em.place(after_null);

    let a_next = em.emit(Instruction::new(Opcode::Next, a_cursor, 0, 0));
    em.patch_p2(a_next, a_loop);
    em.place(a_rewind_end);

    // Pass 2: one `A`-nulled row for every `B` rowid pass 1 never
    // recorded.
    let b2_rewind_end = em.new_label();
    let b2_rewind = em.emit(Instruction::new(Opcode::Rewind, b_cursor, 0, 0));
    em.patch_p2(b2_rewind, b2_rewind_end);
    let b2_loop = em.new_label();
    em.place(b2_loop);
    let b2_skip = em.new_label();
    let rowid2_reg = reg.alloc();
    em.emit(Instruction::new(Opcode::Rowid, b_cursor, rowid2_reg, 0));
    let found_addr = em.emit(Instruction::with_p4(
        Opcode::Found,
        eph_cursor,
        0,
        rowid2_reg,
        P4::Int(1),
    ));
    em.patch_p2(found_addr, b2_skip);
    let mut a_null_bindings = bindings.clone();
    if let Some(a) = a_null_bindings.get_mut(0) {
        a.forced_null = true;
    }
    let a_null_scope = Scope {
        tables: a_null_bindings,
        catalog: schemas.to_vec(),
        outer: None,
        dedup_star: dedup_star.clone(),
    };
    emit_join_final_row(
        &mut em,
        &mut reg,
        select,
        &a_null_scope,
        end_label,
        limit.as_ref(),
        None,
        &mut sink,
    )?;
    em.place(b2_skip);
    let b2_next = em.emit(Instruction::new(Opcode::Next, b_cursor, 0, 0));
    em.patch_p2(b2_next, b2_loop);
    em.place(b2_rewind_end);

    em.place(end_label);
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    Ok(em.finish())
}
