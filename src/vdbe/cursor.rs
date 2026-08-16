//! Cursor opcodes (spec 009, Requirement 4): real table cursors over
//! V1's `TableCursor` (`OpenRead`/`Rewind`/`Last`/`Next`/`Column`/
//! `Rowid`/`SeekRowid`/`NullRow`), an in-memory ephemeral index for
//! DISTINCT (`OpenEphemeral`/`Sequence`/`Found`/`IdxInsert`/`IdxLE`/
//! `Delete`, per the epic's #87 scope decision — never the on-disk file
//! format), and a single-row pseudo-cursor (`OpenPseudo`) that lets
//! `Column` read an already-computed record (the sorter's output row)
//! without a special case.
//!
//! Register/cursor-slot conventions used by this module's opcodes (this
//! ticket's own choice — codegen, #91, is what will actually decide
//! operand layout against the pinned oracle's `EXPLAIN` output; nothing
//! here claims byte-for-byte parity with a harvested instruction's
//! P1..P5, only with the opcode's *semantics*):
//! - `OpenRead(p1=cursor, p2=root page)`
//! - `OpenEphemeral(p1=cursor)` — key-column count isn't needed by this
//!   in-memory implementation (the whole register range passed to
//!   `Found`/`IdxInsert` *is* the key), so `P2` is unused here.
//! - `OpenPseudo(p1=cursor, p2=register holding the row's record blob)`
//! - `Rewind`/`Last(p1=cursor, p2=jump target if the table is empty)`
//! - `Next(p1=cursor, p2=jump target if another row was found)` —
//!   mirrors the oracle's own `OP_Next` shape: jump back into the loop
//!   body on success, fall through to end the loop on exhaustion.
//! - `Column(p1=cursor, p2=column index, p3=dest register)`
//! - `Rowid(p1=cursor, p2=dest register)`
//! - `SeekRowid(p1=cursor, p2=jump target if not found, p3=register
//!   holding the target rowid)`
//! - `NullRow(p1=cursor)`
//! - `Sequence(p1=cursor, p2=dest register)`
//! - `Found(p1=cursor, p2=jump target if the key is present, p3=first
//!   key register, p4=Int(key column count))`
//! - `IdxInsert(p1=cursor, p2=first key register, p4=Int(key column
//!   count))`
//! - `IdxLE(p1=cursor, p2=jump target, p3=first key register,
//!   p4=Int(key column count))` — see [`idx_le`]'s doc for this
//!   opcode's known scope limitation.
//! - `Delete(p1=cursor)` — deletes the entry `Found`/`IdxInsert` most
//!   recently probed/inserted on this cursor.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::btree::{TableCursor, TableRow};
use crate::record::{decode_record, encode_record, TextEncoding, Value};
use crate::vdbe::exec::{to_pc, ExecError, Step, Vm};
use crate::vdbe::program::{Instruction, P4};

/// One open cursor slot: a real table cursor, an in-memory ephemeral
/// index, a sorter (state owned by `src/vdbe/sorter.rs`, re-exported
/// here so `Vm`'s single cursor-slot table can hold all cursor kinds),
/// or a single-row pseudo-cursor.
#[derive(Debug)]
pub(crate) enum CursorSlot {
    Table(TableCursorState),
    Ephemeral(EphemeralState),
    Pseudo { register: i32 },
    Sorter(crate::vdbe::sorter::SorterState),
}

impl CursorSlot {
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            CursorSlot::Table(_) => "table cursor",
            CursorSlot::Ephemeral(_) => "ephemeral cursor",
            CursorSlot::Pseudo { .. } => "pseudo cursor",
            CursorSlot::Sorter(_) => "sorter cursor",
        }
    }
}

/// A real cursor over `src/btree`'s table b-tree, plus the traversal
/// state `Column`/`Rowid` read from: the row `Rewind`/`Next`/`Last`/
/// `SeekRowid` most recently positioned on (`None` once exhausted), and
/// whether `NullRow` has forced this slot to read as an all-NULL row
/// (used by e.g. an outer-join-style probe that found no match — `Next`/
/// `Rewind` clear it again on the next real positioning call).
pub(crate) struct TableCursorState {
    cursor: TableCursor<Rc<dyn crate::vfs::PageSource>>,
    current: Option<TableRow>,
    forced_null: bool,
}

impl std::fmt::Debug for TableCursorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TableCursorState")
            .field("current", &self.current)
            .field("forced_null", &self.forced_null)
            .finish_non_exhaustive()
    }
}

/// The in-memory `BTreeMap` backing DISTINCT's ephemeral index (#87):
/// entries keyed by the encoded record bytes of the probed/inserted
/// column range (spec 003's format, reused byte-for-byte — same encoder
/// `MakeRecord` uses), never touching the on-disk page format.
/// `sequence` is a monotonic counter `Sequence` hands out (independent
/// of the dedup key), and `last_key` records the key `Found`/`IdxInsert`
/// most recently touched, so a following `Delete` (per spec 009 Req 4's
/// "insert then delete the just-produced duplicate" DISTINCT dance)
/// knows which entry to remove without a separate register operand.
#[derive(Debug, Default)]
pub(crate) struct EphemeralState {
    entries: BTreeMap<Vec<u8>, Vec<Value>>,
    sequence: i64,
    last_key: Option<Vec<u8>>,
}

fn table_cursor_mut<'v>(
    vm: &'v mut Vm,
    slot: i32,
    opcode: &'static str,
) -> Result<&'v mut TableCursorState, ExecError> {
    match vm.cursor_mut(slot)? {
        CursorSlot::Table(state) => Ok(state),
        other => Err(ExecError::CursorTypeMismatch {
            opcode,
            slot,
            found: other.type_name(),
            expected: "table cursor",
        }),
    }
}

fn ephemeral_mut<'v>(
    vm: &'v mut Vm,
    slot: i32,
    opcode: &'static str,
) -> Result<&'v mut EphemeralState, ExecError> {
    match vm.cursor_mut(slot)? {
        CursorSlot::Ephemeral(state) => Ok(state),
        other => Err(ExecError::CursorTypeMismatch {
            opcode,
            slot,
            found: other.type_name(),
            expected: "ephemeral cursor",
        }),
    }
}

fn p4_count(instr: &Instruction, opcode: &'static str) -> Result<usize, ExecError> {
    match &instr.p4 {
        P4::Int(n) => usize::try_from(*n).map_err(|_| ExecError::MalformedInstruction {
            opcode,
            reason: format!("negative key column count {n}"),
        }),
        other => Err(ExecError::MalformedInstruction {
            opcode,
            reason: format!("expected an integer P4 (key column count), got {other:?}"),
        }),
    }
}

fn read_register_range(
    vm: &Vm,
    start: i32,
    count: usize,
    opcode: &'static str,
) -> Result<Vec<Value>, ExecError> {
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        let reg = start
            .checked_add(
                i32::try_from(i).map_err(|_| ExecError::RegisterRangeTooLarge {
                    opcode,
                    count: count as i32,
                })?,
            )
            .ok_or(ExecError::RegisterOutOfRange {
                opcode,
                index: start,
            })?;
        values.push(vm.register(reg)?.clone());
    }
    Ok(values)
}

/// `OpenRead`: opens a real read cursor on `P2` (the table's root page)
/// into cursor slot `P1`, sharing the `Vm`'s attached database page
/// source (see `Vm::with_db`) with every other open `OpenRead` cursor.
pub fn open_read(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let root_page = u32::try_from(instr.p2).map_err(|_| ExecError::MalformedInstruction {
        opcode: "OpenRead",
        reason: format!("invalid root page {}", instr.p2),
    })?;
    let db = vm.db()?;
    let cursor = TableCursor::new(Rc::clone(&db.source), &db.header, root_page);
    vm.set_cursor(
        instr.p1,
        CursorSlot::Table(TableCursorState {
            cursor,
            current: None,
            forced_null: false,
        }),
    )?;
    Ok(Step::Next)
}

/// `OpenEphemeral`: opens an empty in-memory ephemeral index (DISTINCT's
/// dedup table, #87) into cursor slot `P1`.
pub fn open_ephemeral(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    vm.set_cursor(instr.p1, CursorSlot::Ephemeral(EphemeralState::default()))?;
    Ok(Step::Next)
}

/// `OpenPseudo`: opens a single-row pseudo-cursor into slot `P1` that
/// re-presents register `P2`'s record blob as a cursor row, so `Column`
/// needs no special case for sorter-sourced (or otherwise
/// already-computed) rows.
pub fn open_pseudo(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    vm.set_cursor(instr.p1, CursorSlot::Pseudo { register: instr.p2 })?;
    Ok(Step::Next)
}

/// `Rewind`: positions cursor `P1` at its first row, jumping to `P2` if
/// the table is empty (mirrors the oracle's own `OP_Rewind` shape).
pub fn rewind(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = table_cursor_mut(vm, instr.p1, "Rewind")?;
    state.forced_null = false;
    state.current = state
        .cursor
        .first()
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "Rewind",
            reason: e.to_string(),
        })?;
    Ok(if state.current.is_none() {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `Last`: positions cursor `P1` at its last row (highest rowid),
/// jumping to `P2` if the table is empty.
pub fn last(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = table_cursor_mut(vm, instr.p1, "Last")?;
    state.forced_null = false;
    state.current = state
        .cursor
        .last()
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "Last",
            reason: e.to_string(),
        })?;
    Ok(if state.current.is_none() {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `Next`: advances cursor `P1` to the following row, jumping to `P2`
/// (typically back to the loop body's start) if another row was found —
/// falls through (ending the loop) once exhausted.
pub fn next(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = table_cursor_mut(vm, instr.p1, "Next")?;
    state.current = state
        .cursor
        .next()
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "Next",
            reason: e.to_string(),
        })?;
    Ok(if state.current.is_some() {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

fn current_row_columns(vm: &Vm, slot: i32, opcode: &'static str) -> Result<Vec<Value>, ExecError> {
    match vm.cursor(slot)? {
        CursorSlot::Table(state) => {
            if state.forced_null {
                return Ok(Vec::new());
            }
            let row = state
                .current
                .as_ref()
                .ok_or(ExecError::MalformedInstruction {
                    opcode,
                    reason: "cursor has no current row".to_string(),
                })?;
            decode_record(&row.payload, TextEncoding::Utf8).map_err(|e| {
                ExecError::MalformedInstruction {
                    opcode,
                    reason: e.to_string(),
                }
            })
        }
        CursorSlot::Pseudo { register } => {
            let register = *register;
            match vm.register(register)? {
                Value::Blob(bytes) => decode_record(bytes, TextEncoding::Utf8).map_err(|e| {
                    ExecError::MalformedInstruction {
                        opcode,
                        reason: e.to_string(),
                    }
                }),
                other => Err(ExecError::MalformedInstruction {
                    opcode,
                    reason: format!("pseudo-cursor register holds {other:?}, not a record blob"),
                }),
            }
        }
        other => Err(ExecError::CursorTypeMismatch {
            opcode,
            slot,
            found: other.type_name(),
            expected: "table or pseudo cursor",
        }),
    }
}

/// `Column`: reads column `P2` of cursor `P1`'s current row into
/// register `P3`. A `NullRow`-forced table cursor always reads as NULL,
/// regardless of `P2`.
///
/// Known simplification: this does not substitute the rowid-alias
/// column (`INTEGER PRIMARY KEY`, stored as NULL in the record — see
/// `src/btree.rs`'s module doc) with the cursor's actual rowid; that
/// substitution is schema-aware and belongs to codegen (#91), which
/// knows which column, if any, is the alias and can emit `Rowid` instead
/// of `Column` for it.
pub fn column(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let values = current_row_columns(vm, instr.p1, "Column")?;
    let idx = usize::try_from(instr.p2).map_err(|_| ExecError::MalformedInstruction {
        opcode: "Column",
        reason: format!("negative column index {}", instr.p2),
    })?;
    let value = values.get(idx).cloned().unwrap_or(Value::Null);
    vm.set_register(instr.p3, value)?;
    Ok(Step::Next)
}

/// `Rowid`: writes cursor `P1`'s current rowid into register `P2` (NULL
/// if the cursor is `NullRow`-forced).
pub fn rowid(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let value = match vm.cursor(instr.p1)? {
        CursorSlot::Table(state) => {
            if state.forced_null {
                Value::Null
            } else {
                let row = state
                    .current
                    .as_ref()
                    .ok_or(ExecError::MalformedInstruction {
                        opcode: "Rowid",
                        reason: "cursor has no current row".to_string(),
                    })?;
                Value::Integer(row.rowid)
            }
        }
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode: "Rowid",
                slot: instr.p1,
                found: other.type_name(),
                expected: "table cursor",
            })
        }
    };
    vm.set_register(instr.p2, value)?;
    Ok(Step::Next)
}

/// `SeekRowid`: positions cursor `P1` at the row whose rowid equals
/// register `P3`, jumping to `P2` if no such row exists.
pub fn seek_rowid(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let target = match vm.register(instr.p3)? {
        Value::Integer(i) => *i,
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "SeekRowid",
                reason: format!("target rowid register holds {other:?}, not an integer"),
            })
        }
    };
    let state = table_cursor_mut(vm, instr.p1, "SeekRowid")?;
    state.forced_null = false;
    state.current = state
        .cursor
        .seek(target)
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "SeekRowid",
            reason: e.to_string(),
        })?;
    Ok(if state.current.is_none() {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `NullRow`: forces cursor `P1` to read as an all-NULL row until its
/// next real positioning (`Rewind`/`Last`/`Next`/`SeekRowid`).
pub fn null_row(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = table_cursor_mut(vm, instr.p1, "NullRow")?;
    state.forced_null = true;
    state.current = None;
    Ok(Step::Next)
}

/// `Sequence`: writes ephemeral cursor `P1`'s next monotonic counter
/// value into register `P2` (independent of the dedup key — used to
/// allocate a synthetic rowid for an ephemeral-table row).
pub fn sequence(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let value = {
        let state = ephemeral_mut(vm, instr.p1, "Sequence")?;
        let v = state.sequence;
        state.sequence = state.sequence.saturating_add(1);
        v
    };
    vm.set_register(instr.p2, Value::Integer(value))?;
    Ok(Step::Next)
}

/// `Found`: probes ephemeral cursor `P1` for the key built from `P4`
/// (`Int`, the key column count) registers starting at `P3`, jumping to
/// `P2` if present. Either way, remembers the probed key as the target
/// of a following `IdxInsert`/`Delete`.
pub fn found(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = p4_count(instr, "Found")?;
    let values = read_register_range(vm, instr.p3, count, "Found")?;
    let key = encode_record(&values, TextEncoding::Utf8);
    let state = ephemeral_mut(vm, instr.p1, "Found")?;
    let present = state.entries.contains_key(&key);
    state.last_key = Some(key);
    Ok(if present {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `IdxInsert`: inserts the key built from `P4` (`Int`, the key column
/// count) registers starting at `P2` into ephemeral cursor `P1`.
pub fn idx_insert(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = p4_count(instr, "IdxInsert")?;
    let values = read_register_range(vm, instr.p2, count, "IdxInsert")?;
    let key = encode_record(&values, TextEncoding::Utf8);
    let state = ephemeral_mut(vm, instr.p1, "IdxInsert")?;
    state.entries.insert(key.clone(), values);
    state.last_key = Some(key);
    Ok(Step::Next)
}

/// `IdxLE`: jumps to `P2` if the key built from `P4` (`Int`, the key
/// column count) registers starting at `P3` is `<=` ephemeral cursor
/// `P1`'s most recently probed/inserted key (byte-order comparison of
/// the encoded key, matching a BINARY-collated index).
///
/// Known scope limitation: the harvested use of this opcode
/// (`tools/opcodes-v2.json`) ties it to an `ORDER BY ... LIMIT 1`
/// index-seek query-planner optimization this ticket does not
/// implement (the full sorter path, Requirement 9, is what V2 actually
/// executes for `ORDER BY`) — there is no harvested example with more
/// than one occurrence to derive a fuller semantics from. This
/// implementation gives `IdxLE` a well-defined, testable meaning against
/// the same ephemeral cursor `Found`/`IdxInsert` already use, rather
/// than leaving it unimplemented, but does not claim oracle-exact parity
/// for the optimization the harvest actually observed it in.
pub fn idx_le(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = p4_count(instr, "IdxLE")?;
    let values = read_register_range(vm, instr.p3, count, "IdxLE")?;
    let probe = encode_record(&values, TextEncoding::Utf8);
    let state = ephemeral_mut(vm, instr.p1, "IdxLE")?;
    let holds = match &state.last_key {
        Some(key) => *key <= probe,
        None => true,
    };
    Ok(if holds {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `Delete`: removes ephemeral cursor `P1`'s most recently probed/
/// inserted entry (per `Found`/`IdxInsert`'s `last_key`) — DISTINCT's
/// "insert then delete the just-produced duplicate" path (spec 009
/// Requirement 4).
pub fn delete(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = ephemeral_mut(vm, instr.p1, "Delete")?;
    if let Some(key) = state.last_key.take() {
        state.entries.remove(&key);
    }
    Ok(Step::Next)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::header::DatabaseHeader;
    use crate::vdbe::program::Opcode;
    use crate::vfs::{UnixVfs, Vfs, VfsPageSource};
    use std::path::Path;

    fn open_vm(fixture: &str) -> Vm {
        let path = Path::new("tests/corpus/fixtures/btrees").join(fixture);
        let vfs = UnixVfs;
        let file = vfs.open_read(&path).unwrap();
        let mut header_buf = [0u8; 100];
        file.read_at(&mut header_buf, 0).unwrap();
        let header = DatabaseHeader::parse(&header_buf).unwrap();
        let source = VfsPageSource::open(&vfs, &path, header.page_size).unwrap();
        Vm::with_db(Rc::new(source), header)
    }

    #[test]
    fn full_scan_opens_rewinds_iterates_reads() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();

        let step = rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 999, 0)).unwrap();
        assert_eq!(step, Step::Next);

        let mut rowids = Vec::new();
        loop {
            rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 10, 0)).unwrap();
            rowids.push(vm.register(10).unwrap().clone());
            column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 11)).unwrap();

            match next(&mut vm, &Instruction::new(Opcode::Next, 0, 1, 0)).unwrap() {
                Step::Jump(1) => continue,
                Step::Next => break,
                other => panic!("unexpected step {other:?}"),
            }
        }

        assert_eq!(rowids.len(), 3000);
        assert_eq!(rowids[0], Value::Integer(1));
        assert_eq!(rowids[2999], Value::Integer(3000));
        assert_eq!(
            *vm.register(11).unwrap(),
            Value::Text("row number 3000".to_string())
        );
    }

    #[test]
    fn seek_rowid_jumps_to_p2_when_the_target_rowid_is_absent() {
        let mut vm = open_vm("table_single_page.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        vm.set_register(5, Value::Integer(999)).unwrap();
        let step = seek_rowid(&mut vm, &Instruction::new(Opcode::SeekRowid, 0, 42, 5)).unwrap();
        assert_eq!(step, Step::Jump(42));
    }

    #[test]
    fn seek_rowid_skips_full_scan_on_pk_equality() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        vm.set_register(5, Value::Integer(1500)).unwrap();
        let step = seek_rowid(&mut vm, &Instruction::new(Opcode::SeekRowid, 0, 42, 5)).unwrap();
        assert_eq!(step, Step::Next);
        rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 10, 0)).unwrap();
        assert_eq!(*vm.register(10).unwrap(), Value::Integer(1500));
    }

    #[test]
    fn null_row_forces_all_null_reads_until_repositioned() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 999, 0)).unwrap();
        null_row(&mut vm, &Instruction::new(Opcode::NullRow, 0, 0, 0)).unwrap();

        column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 11)).unwrap();
        assert_eq!(*vm.register(11).unwrap(), Value::Null);
        rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 12, 0)).unwrap();
        assert_eq!(*vm.register(12).unwrap(), Value::Null);
    }

    #[test]
    fn distinct_probes_ephemeral_index_before_emit() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 1, 0)).unwrap();

        // Row "a": not found, insert, passes through.
        vm.set_register(0, Value::Text("a".to_string())).unwrap();
        let found_a = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_a, Step::Next);
        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)),
        )
        .unwrap();

        // Row "a" again: found, discard (the DISTINCT dedup path).
        let found_a_again = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_a_again, Step::Jump(99));

        // Row "b": not found, insert, passes through.
        vm.set_register(0, Value::Text("b".to_string())).unwrap();
        let found_b = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_b, Step::Next);
    }

    #[test]
    fn sequence_hands_out_a_monotonic_counter_independent_of_the_dedup_key() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 1, 0)).unwrap();
        sequence(&mut vm, &Instruction::new(Opcode::Sequence, 0, 5, 0)).unwrap();
        assert_eq!(*vm.register(5).unwrap(), Value::Integer(0));
        sequence(&mut vm, &Instruction::new(Opcode::Sequence, 0, 6, 0)).unwrap();
        assert_eq!(*vm.register(6).unwrap(), Value::Integer(1));
    }

    #[test]
    fn delete_removes_the_just_probed_duplicate_row() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 1, 0)).unwrap();
        vm.set_register(0, Value::Text("a".to_string())).unwrap();
        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)),
        )
        .unwrap();
        found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        delete(&mut vm, &Instruction::new(Opcode::Delete, 0, 0, 0)).unwrap();

        let found_again = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_again, Step::Next);
    }

    #[test]
    fn cursor_type_mismatch_errors_instead_of_panicking() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 1, 0)).unwrap();
        let err = rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 5, 0)).unwrap_err();
        assert!(matches!(err, ExecError::CursorTypeMismatch { .. }));
    }

    #[test]
    fn unopened_cursor_slot_errors_instead_of_panicking() {
        let mut vm = Vm::new();
        let err = rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 5, 0)).unwrap_err();
        assert!(matches!(err, ExecError::CursorNotOpen { slot: 0 }));
    }
}
