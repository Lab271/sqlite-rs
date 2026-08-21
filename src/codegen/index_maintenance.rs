//! Secondary-index maintenance shared by `INSERT`/`DELETE`/`UPDATE`
//! codegen (#196): open a write cursor per index alongside the table
//! cursor, and emit the `IdxInsert`/`IdxDelete` pair for a row's index
//! entries.
//!
//! Index keys are read back from the table cursor's *current* row via
//! ordinary `Opcode::Column`/`Opcode::Rowid` (rowid last, matching the
//! on-disk index key convention `btree::index::insert`/`index::delete`
//! use) rather than reusing already-computed value registers — there is
//! no register-copy opcode in the frozen V2 set
//! (`tools/opcodes-v2.json`), so a fresh contiguous register run is
//! rebuilt by reading from a cursor every time one is needed. For a
//! freshly-inserted/updated row, the caller must position the table
//! cursor on it first (`SeekRowid`) since `Opcode::Insert` does not
//! reposition the cursor itself.
//!
//! `DESC` index columns are rejected (`CodegenError::Unsupported`)
//! rather than silently mis-keyed: no index b-tree comparator in this
//! codebase (#171) is aware of per-column sort direction, so a `DESC`
//! column would otherwise get built into the key as if it were
//! ascending — a plausible-looking but semantically backwards key.

use crate::codegen::expr::{column_index, emit_column_read};
use crate::codegen::select::CodegenError;
use crate::codegen::{Emitter, RegAlloc};
use crate::schema::TableSchema;
use crate::vdbe::{Instruction, Opcode, P4};

/// `OpenWrite`s one write cursor per index on `schema`, starting at
/// `first_cursor`, with `P5 = 1` selecting `CursorSlot::IndexWrite`
/// (#194's `OpenWrite` doc).
///
/// `sqlite_master.rootpage` is untrusted on-disk data — a corrupt or
/// adversarial file could carry a zero, negative, or out-of-`i32`-range
/// value for it. Rather than defaulting a bad value to page 0 (the
/// reserved header page) and silently pointing the write cursor at it,
/// this rejects the schema outright.
pub(crate) fn open_index_cursors(
    em: &mut Emitter,
    schema: &TableSchema,
    first_cursor: i32,
) -> Result<(), CodegenError> {
    for (i, index) in schema.indexes.iter().enumerate() {
        let cursor = first_cursor.saturating_add(i32::try_from(i).unwrap_or(0));
        let root_page = i32::try_from(index.root_page)
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| CodegenError::Unsupported {
                reason: format!(
                    "index {} has an invalid root page ({})",
                    index.name, index.root_page
                ),
            })?;
        let mut instr = Instruction::new(Opcode::OpenWrite, cursor, root_page, 0);
        instr.p5 = 1;
        em.emit(instr);
    }
    Ok(())
}

/// For every index on `schema`, reads the current row at `table_cursor`
/// into a fresh contiguous register block (index columns in declared
/// order, then rowid) and emits `opcode` (`IdxInsert` or `IdxDelete`)
/// against the matching cursor in `[first_index_cursor, ...)`.
///
/// The table cursor must already be positioned on the row whose index
/// entries are being built — callers use this both pre-`Delete` (cursor
/// already there) and post-`Insert` (after a `SeekRowid` back onto the
/// just-written row).
pub(crate) fn emit_index_key_ops(
    em: &mut Emitter,
    reg: &mut RegAlloc,
    schema: &TableSchema,
    table_cursor: i32,
    first_index_cursor: i32,
    opcode: Opcode,
) -> Result<(), CodegenError> {
    for (i, index) in schema.indexes.iter().enumerate() {
        let index_cursor = first_index_cursor.saturating_add(i32::try_from(i).unwrap_or(0));
        let mut start = None;
        for col in &index.columns {
            if col.desc {
                // No index b-tree comparator anywhere in this codebase
                // (#171) is aware of per-column sort direction — it
                // always compares the encoded key ascending. Silently
                // building the key as if `col` were ascending would
                // write an index stock `sqlite3` (which does honor
                // `DESC`) reads back in the wrong order. Reject loudly
                // instead of writing a key that's byte-for-byte
                // plausible but semantically backwards.
                return Err(CodegenError::Unsupported {
                    reason: format!(
                        "index {} has a DESC column ({}); descending index keys aren't supported yet",
                        index.name, col.name
                    ),
                });
            }
            let col_idx =
                column_index(schema, &col.name).ok_or_else(|| CodegenError::Unsupported {
                    reason: format!(
                        "index {} references a column or expression this codegen can't resolve: {}",
                        index.name, col.name
                    ),
                })?;
            let r = reg.alloc();
            if start.is_none() {
                start = Some(r);
            }
            emit_column_read(em, schema, table_cursor, col_idx, r)?;
        }
        let rowid_reg = reg.alloc();
        if start.is_none() {
            start = Some(rowid_reg);
        }
        em.emit(Instruction::new(Opcode::Rowid, table_cursor, rowid_reg, 0));

        let count = i32::try_from(index.columns.len().saturating_add(1)).unwrap_or(0);
        em.emit(Instruction::with_p4(
            opcode,
            index_cursor,
            start.unwrap_or(rowid_reg),
            0,
            P4::Int(i64::from(count)),
        ));
    }
    Ok(())
}
