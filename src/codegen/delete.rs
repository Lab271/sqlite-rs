//! `Delete` AST -> `Program` compilation (#210, index maintenance #196).
//! Mirrors `select.rs`'s `Init -> OpenRead -> Rewind -> [WHERE test] ->
//! Next -> Halt` scan shape, swapping `OpenRead` for `OpenWrite` and the
//! result-row emission for a per-index `IdxDelete` plus a table
//! `Delete` per matched row.
//!
//! Safe to delete mid-scan: `TableCursor`'s traversal frames are
//! snapshotted page bytes captured at descent time (`src/btree.rs`'s
//! `Frame::page`), so `Opcode::Delete` mutating the on-disk b-tree via
//! `btree::delete_row` never invalidates this cursor's own in-flight
//! `Next` traversal — unlike a cursor that re-reads live page state on
//! every step.
//!
//! Known simplification: no rowid-equality fast path (`SeekRowid`, as
//! `select.rs`'s `try_compile_rowid_seek` uses) — a `WHERE rowid = ?`
//! `DELETE` still compiles to a full scan. Correct, just not the O(log
//! n) shape; not required by this ticket's oracle-parity acceptance
//! criterion, which only checks value semantics.

use crate::codegen::expr::compile_cond;
use crate::codegen::index_maintenance::{emit_index_key_ops, open_index_cursors};
use crate::codegen::select::CodegenError;
use crate::codegen::{CondTargets, Emitter, RegAlloc, Scope, Target};
use crate::parser::ast::Delete;
use crate::schema::TableSchema;
use crate::vdbe::{Instruction, Opcode, Program};

const TABLE_CURSOR: i32 = 0;
const FIRST_INDEX_CURSOR: i32 = 1;

/// Compiles `delete` against `schema` (the resolved target table) into
/// a `Program`.
pub fn compile_delete(delete: &Delete, schema: &TableSchema) -> Result<Program, CodegenError> {
    if schema.without_rowid {
        return Err(CodegenError::Unsupported {
            reason: "WITHOUT ROWID tables are not supported by DELETE codegen yet".to_string(),
        });
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
    open_index_cursors(&mut em, schema, FIRST_INDEX_CURSOR)?;

    let end_label = em.new_label();
    let rewind_addr = em.emit(Instruction::new(Opcode::Rewind, TABLE_CURSOR, 0, 0));
    em.patch_p2(rewind_addr, end_label);
    let loop_start = em.new_label();
    em.place(loop_start);

    let row_skip = em.new_label();
    if let Some(where_expr) = &delete.where_clause {
        compile_cond(
            &mut em,
            &mut reg,
            &Scope::single(schema, TABLE_CURSOR),
            where_expr,
            CondTargets::null_is_false(Target::Fallthrough, Target::Jump(row_skip)),
        )?;
    }

    emit_index_key_ops(
        &mut em,
        &mut reg,
        schema,
        TABLE_CURSOR,
        FIRST_INDEX_CURSOR,
        Opcode::IdxDelete,
    )?;
    em.emit(Instruction::new(Opcode::Delete, TABLE_CURSOR, 0, 0));

    em.place(row_skip);
    let next_addr = em.emit(Instruction::new(Opcode::Next, TABLE_CURSOR, 0, 0));
    em.patch_p2(next_addr, loop_start);

    em.place(end_label);
    em.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    Ok(em.finish())
}
