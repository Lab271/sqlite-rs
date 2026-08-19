//! `Update` AST -> `Program` compilation (#210): table-only path, no
//! secondary-index maintenance (that's #196). Mirrors `delete.rs`'s
//! scan shape, but per matched row builds the new record from a mix of
//! assigned expressions and the row's own unassigned columns (read via
//! `emit_column_read` before the row is touched), then emits `Delete`
//! + `Insert` rather than `Delete` alone — SQLite's own "no in-place
//!   update opcode" convention for a b-tree keyed by rowid, since a
//!   `SET`-assignment to the rowid-alias column can change the row's key.
//!
//! Known simplifications (deferred to follow-up tickets, not chased
//! here): no NOT NULL/CHECK/DEFAULT constraint re-validation (`INSERT`'s
//! `column_plans` machinery is not reused) and no rowid-equality
//! `SeekRowid` fast path — both are correctness-neutral simplifications
//! `insert.rs`/`select.rs` already document precedent for.

use crate::codegen::expr::{column_index, compile_cond, compile_value, emit_column_read};
use crate::codegen::select::CodegenError;
use crate::codegen::{CondTargets, Emitter, RegAlloc, Target};
use crate::parser::ast::{Expr, Update};
use crate::schema::{rowid_alias_column, TableSchema};
use crate::vdbe::{affinity_of, Instruction, Opcode, Program, P4};

const TABLE_CURSOR: i32 = 0;

/// Compiles `update` against `schema` (the resolved target table) into
/// a `Program`.
pub fn compile_update(update: &Update, schema: &TableSchema) -> Result<Program, CodegenError> {
    if schema.without_rowid {
        return Err(CodegenError::Unsupported {
            reason: "WITHOUT ROWID tables are not supported by UPDATE codegen yet".to_string(),
        });
    }

    let rowid_alias = rowid_alias_column(schema);
    let mut assigned: Vec<Option<&Expr>> = vec![None; schema.columns.len()];
    for assignment in &update.assignments {
        for name in &assignment.columns {
            let idx = column_index(schema, name)
                .ok_or_else(|| CodegenError::UnknownColumn { name: name.clone() })?;
            if let Some(slot) = assigned.get_mut(idx) {
                *slot = Some(&assignment.value);
            }
        }
    }

    let mut em = Emitter::new();
    let mut reg = RegAlloc::new();

    let init_addr = em.emit(Instruction::new(Opcode::Init, 0, 0, 0));
    let body_start = em.new_label();
    em.place(body_start);
    em.patch_p2(init_addr, body_start);

    em.emit(Instruction::new(
        Opcode::OpenWrite,
        TABLE_CURSOR,
        i32::try_from(schema.root_page).unwrap_or(0),
        0,
    ));

    let end_label = em.new_label();
    let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, TABLE_CURSOR, 0, 0));
    em.patch_p2(rewind_addr, end_label);
    let loop_start = em.new_label();
    em.place(loop_start);

    let row_skip = em.new_label();
    if let Some(where_expr) = &update.where_clause {
        compile_cond(
            &mut em,
            &mut reg,
            schema,
            TABLE_CURSOR,
            where_expr,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(row_skip)),
        )?;
    }

    // Every value the new row needs — including a possibly-reassigned
    // rowid — is read from the cursor's *current* row before `Delete`
    // below clears it (`cursor::delete` sets `state.current = None`).
    let rowid_reg = match rowid_alias.and_then(|idx| assigned.get(idx).copied().flatten()) {
        Some(expr) => compile_value(&mut em, &mut reg, schema, TABLE_CURSOR, expr)?,
        None => {
            let r = reg.alloc();
            em.emit(Instruction::new(Opcode::Rowid, TABLE_CURSOR, r, 0));
            r
        }
    };

    let mut col_regs = Vec::with_capacity(schema.columns.len());
    for (idx, expr) in assigned.iter().enumerate() {
        if Some(idx) == rowid_alias {
            let r = reg.alloc();
            em.emit(Instruction::new(Opcode::Null, 0, r, 0));
            col_regs.push(r);
            continue;
        }
        let r = match expr {
            Some(expr) => compile_value(&mut em, &mut reg, schema, TABLE_CURSOR, expr)?,
            None => {
                let r = reg.alloc();
                emit_column_read(&mut em, schema, TABLE_CURSOR, idx, r)?;
                r
            }
        };
        col_regs.push(r);
    }

    let base_reg = col_regs.first().copied().unwrap_or(0);
    let count = i32::try_from(col_regs.len()).unwrap_or(0);
    let record_reg = reg.alloc();
    let affinities: Vec<u8> = schema
        .column_types
        .iter()
        .map(|t| affinity_of(t).to_p4_byte())
        .collect();
    em.emit(Instruction::with_p4(
        Opcode::MakeRecord,
        base_reg,
        count,
        record_reg,
        P4::Affinity(affinities),
    ));

    em.emit(Instruction::new(Opcode::Delete, TABLE_CURSOR, 0, 0));
    em.emit(Instruction::new(
        Opcode::Insert,
        TABLE_CURSOR,
        rowid_reg,
        record_reg,
    ));

    em.place(row_skip);
    let next_addr = em.emit(Instruction::new(Opcode::Next, TABLE_CURSOR, 0, 0));
    em.patch_p2(next_addr, loop_start);

    em.place(end_label);
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    Ok(em.finish())
}
