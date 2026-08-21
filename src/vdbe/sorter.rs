//! Sorter opcodes (spec 009, Requirement 9): ORDER BY as its own
//! in-memory opcode family, distinct from cursor scanning. Every
//! candidate row is buffered via `SorterInsert` during an initial scan
//! pass; `SorterSort`/`Sort` then sorts the buffer once, keyed by the
//! `P4` sort-key descriptor (`SortKeyColumn`s: per-column direction and
//! collation, applied to the record's leading columns — the row's sort
//! key is expected to be encoded first in the `MakeRecord`'d payload
//! `SorterInsert` receives, matching the oracle's own emission shape);
//! `SorterNext`/`SorterData` then iterate the sorted result exactly as
//! `Next`/`Column` iterate a table cursor, feeding an `OpenPseudo`
//! cursor (`src/vdbe/cursor.rs`) so downstream `Column` opcodes need no
//! special case for sorter-sourced rows.
//!
//! Register/cursor-slot conventions this ticket chose (see
//! `src/vdbe/cursor.rs`'s module doc for the same caveat: these are not
//! claimed to match codegen's, #91, eventual harvested operand layout):
//! - `SorterOpen(p1=cursor, p4=SortKey(columns))`
//! - `SorterInsert(p1=cursor, p2=register holding the record blob)`
//! - `SorterSort`/`Sort(p1=cursor, p2=jump target if the sorter is
//!   empty)` — mirrors `Rewind`'s "jump on empty" shape.
//! - `SorterNext(p1=cursor, p2=jump target if another row was found)` —
//!   mirrors `Next`'s "jump on success" shape.
//! - `SorterData(p1=cursor, p2=dest register)` — writes the current
//!   sorted row's raw record bytes (a `Value::Blob`) into the register,
//!   the same register `OpenPseudo`'s `P2` names.

use std::cmp::Ordering;

use crate::record::{decode_record, TextEncoding, Value};
use crate::vdbe::compare::compare;
use crate::vdbe::cursor::CursorSlot;
use crate::vdbe::exec::{to_pc, ExecError, Step, Vm};
use crate::vdbe::program::{Instruction, SortKeyColumn, P4};

/// A sorter cursor's state: the sort-key descriptor, the buffered raw
/// record bytes (unsorted until `SorterSort` runs), and the current
/// iteration position once sorted.
#[derive(Debug)]
pub(crate) struct SorterState {
    keys: Vec<SortKeyColumn>,
    buffer: Vec<Vec<u8>>,
    sorted: bool,
    pos: usize,
}

// Methods rather than free functions so the borrow of `self` elides: a free
// `fn(vm: &Vm, opcode: &'static str) -> Result<&SorterState, _>` has two input
// lifetime positions and so needs an explicit parameter, which is outside the
// qualified subset (`make mvl-limit`). The `&self` elision rule resolves it.
impl Vm {
    fn sorter_mut(
        &mut self,
        slot: i32,
        opcode: &'static str,
    ) -> Result<&mut SorterState, ExecError> {
        match self.cursor_mut(slot)? {
            CursorSlot::Sorter(state) => Ok(state),
            other => Err(ExecError::CursorTypeMismatch {
                opcode,
                slot,
                found: other.type_name(),
                expected: "sorter cursor",
            }),
        }
    }

    fn sorter_ref(&self, slot: i32, opcode: &'static str) -> Result<&SorterState, ExecError> {
        match self.cursor(slot)? {
            CursorSlot::Sorter(state) => Ok(state),
            other => Err(ExecError::CursorTypeMismatch {
                opcode,
                slot,
                found: other.type_name(),
                expected: "sorter cursor",
            }),
        }
    }
}

/// `SorterOpen`: opens an empty sorter, keyed by `P4`'s sort-key
/// descriptor, into cursor slot `P1`.
pub fn sorter_open(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let keys = match &instr.p4 {
        P4::SortKey(keys) => keys.clone(),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "SorterOpen",
                reason: format!("expected a SortKey P4, got {other:?}"),
            })
        }
    };
    vm.set_cursor(
        instr.p1,
        CursorSlot::Sorter(SorterState {
            keys,
            buffer: Vec::new(),
            sorted: false,
            pos: 0,
        }),
    )?;
    Ok(Step::Next)
}

/// `SorterInsert`: buffers register `P2`'s record blob as one candidate
/// row on sorter `P1`. Invalidates any prior sort — the buffer is
/// re-sorted the next time `SorterSort`/`Sort` runs.
pub fn sorter_insert(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let blob = match vm.register(instr.p2)? {
        Value::Blob(b) => b.clone(),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "SorterInsert",
                reason: format!("expected a record Blob, got {other:?}"),
            })
        }
    };
    let state = vm.sorter_mut(instr.p1, "SorterInsert")?;
    state.buffer.push(blob.to_vec());
    state.sorted = false;
    Ok(Step::Next)
}

/// `SorterSort`/`Sort`: sorts sorter `P1`'s buffered rows in place by
/// its key descriptor's per-column direction and collation, delegating
/// the actual value comparison to the kernel (spec 009 Requirement 5).
/// Jumps to `P2` if the sorter is empty (mirrors `Rewind`).
pub fn sorter_sort(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = vm.sorter_mut(instr.p1, "SorterSort")?;
    let keys = state.keys.clone();
    let mut decoded = Vec::with_capacity(state.buffer.len());
    for bytes in &state.buffer {
        let values = decode_record(bytes, TextEncoding::Utf8).map_err(|e| {
            ExecError::MalformedInstruction {
                opcode: "SorterSort",
                reason: e.to_string(),
            }
        })?;
        decoded.push((values, bytes.clone()));
    }
    decoded.sort_by(|(a, _), (b, _)| {
        for key in &keys {
            let av = a.first_n(key);
            let bv = b.first_n(key);
            let ord = match (av, bv) {
                (Value::Null, Value::Null) => Ordering::Equal,
                (Value::Null, _) => {
                    if key.nulls_first {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (_, Value::Null) => {
                    if key.nulls_first {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                _ => {
                    let ord = compare(av, bv, key.collation);
                    if key.descending {
                        ord.reverse()
                    } else {
                        ord
                    }
                }
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    state.buffer = decoded.into_iter().map(|(_, bytes)| bytes).collect();
    state.sorted = true;
    state.pos = 0;
    Ok(if state.buffer.is_empty() {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `SorterNext`: advances sorter `P1` to its next sorted row, jumping to
/// `P2` (typically back to the loop body's start) if another row
/// remains — falls through once exhausted, mirroring `Next`.
pub fn sorter_next(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = vm.sorter_mut(instr.p1, "SorterNext")?;
    state.pos = state.pos.saturating_add(1);
    Ok(if state.pos < state.buffer.len() {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `SorterData`: writes sorter `P1`'s current sorted row's raw record
/// bytes into register `P2` — paired with an `OpenPseudo` cursor whose
/// `P2` names the same register, so `Column` can read the row without a
/// sorter-specific case.
pub fn sorter_data(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let bytes = {
        let state = vm.sorter_ref(instr.p1, "SorterData")?;
        if !state.sorted {
            return Err(ExecError::MalformedInstruction {
                opcode: "SorterData",
                reason: "sorter has not been sorted yet (SorterSort/Sort must run first)"
                    .to_string(),
            });
        }
        state
            .buffer
            .get(state.pos)
            .ok_or(ExecError::MalformedInstruction {
                opcode: "SorterData",
                reason: "sorter cursor has no current row".to_string(),
            })?
            .clone()
    };
    vm.set_register(instr.p2, Value::Blob(bytes.into()))?;
    Ok(Step::Next)
}

/// Small helper trait so `sorter_sort`'s comparator can read "the value
/// at this key's column index, or NULL if the row is shorter than the
/// key descriptor implies" without a separate free function shadowing
/// `Vec<Value>::get`.
trait FirstN {
    fn first_n(&self, key: &SortKeyColumn) -> &Value;
}

impl FirstN for Vec<Value> {
    fn first_n(&self, key: &SortKeyColumn) -> &Value {
        self.get(key.index).unwrap_or(&Value::Null)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::record::encode_record;
    use crate::vdbe::collation::Collation;
    use crate::vdbe::program::Opcode;

    fn insert_row(vm: &mut Vm, cursor: i32, values: &[Value]) {
        let blob = encode_record(values, TextEncoding::Utf8);
        vm.set_register(0, Value::Blob(blob.into())).unwrap();
        sorter_insert(vm, &Instruction::new(Opcode::SorterInsert, cursor, 0, 0)).unwrap();
    }

    fn open_sorter(vm: &mut Vm, cursor: i32, keys: Vec<SortKeyColumn>) {
        sorter_open(
            vm,
            &Instruction::with_p4(Opcode::SorterOpen, cursor, 0, 0, P4::SortKey(keys)),
        )
        .unwrap();
    }

    #[test]
    fn order_by_buffers_all_rows_before_sorting() {
        let mut vm = Vm::new();
        open_sorter(
            &mut vm,
            0,
            vec![SortKeyColumn {
                index: 0,
                descending: false,
                collation: Collation::Binary,
                nulls_first: false,
            }],
        );
        insert_row(&mut vm, 0, &[Value::Integer(30)]);
        insert_row(&mut vm, 0, &[Value::Integer(10)]);
        insert_row(&mut vm, 0, &[Value::Integer(20)]);

        let step = sorter_sort(&mut vm, &Instruction::new(Opcode::SorterSort, 0, 999, 0)).unwrap();
        assert_eq!(step, Step::Next);

        let mut seen = Vec::new();
        loop {
            sorter_data(&mut vm, &Instruction::new(Opcode::SorterData, 0, 5, 0)).unwrap();
            let Value::Blob(bytes) = vm.register(5).unwrap() else {
                panic!("expected a Blob");
            };
            let row = decode_record(bytes, TextEncoding::Utf8).unwrap();
            seen.push(row[0].clone());

            match sorter_next(&mut vm, &Instruction::new(Opcode::SorterNext, 0, 1, 0)).unwrap() {
                Step::Jump(1) => continue,
                Step::Next => break,
                other => panic!("unexpected step {other:?}"),
            }
        }

        assert_eq!(
            seen,
            vec![Value::Integer(10), Value::Integer(20), Value::Integer(30)]
        );
    }

    #[test]
    fn sort_key_descriptor_drives_multi_column_order() {
        let mut vm = Vm::new();
        // 2 keys: first descending, second ascending — mirrors the
        // harvested `"k(2,-B,B)"` shape.
        open_sorter(
            &mut vm,
            0,
            vec![
                SortKeyColumn {
                    index: 0,
                    descending: true,
                    collation: Collation::Binary,
                    nulls_first: false,
                },
                SortKeyColumn {
                    index: 1,
                    descending: false,
                    collation: Collation::Binary,
                    nulls_first: false,
                },
            ],
        );
        insert_row(&mut vm, 0, &[Value::Integer(1), Value::Integer(2)]);
        insert_row(&mut vm, 0, &[Value::Integer(2), Value::Integer(1)]);
        insert_row(&mut vm, 0, &[Value::Integer(1), Value::Integer(1)]);

        sorter_sort(&mut vm, &Instruction::new(Opcode::SorterSort, 0, 999, 0)).unwrap();

        let mut seen = Vec::new();
        loop {
            sorter_data(&mut vm, &Instruction::new(Opcode::SorterData, 0, 5, 0)).unwrap();
            let Value::Blob(bytes) = vm.register(5).unwrap() else {
                panic!("expected a Blob");
            };
            seen.push(decode_record(bytes, TextEncoding::Utf8).unwrap());
            match sorter_next(&mut vm, &Instruction::new(Opcode::SorterNext, 0, 1, 0)).unwrap() {
                Step::Jump(1) => continue,
                Step::Next => break,
                other => panic!("unexpected step {other:?}"),
            }
        }

        assert_eq!(
            seen,
            vec![
                vec![Value::Integer(2), Value::Integer(1)],
                vec![Value::Integer(1), Value::Integer(1)],
                vec![Value::Integer(1), Value::Integer(2)],
            ]
        );
    }

    #[test]
    fn empty_sorter_jumps_past_the_loop() {
        let mut vm = Vm::new();
        open_sorter(
            &mut vm,
            0,
            vec![SortKeyColumn {
                index: 0,
                descending: false,
                collation: Collation::Binary,
                nulls_first: false,
            }],
        );
        let step = sorter_sort(&mut vm, &Instruction::new(Opcode::SorterSort, 0, 42, 0)).unwrap();
        assert_eq!(step, Step::Jump(42));
    }

    #[test]
    fn sort_alias_behaves_identically_to_sorter_sort() {
        let mut vm = Vm::new();
        open_sorter(
            &mut vm,
            0,
            vec![SortKeyColumn {
                index: 0,
                descending: false,
                collation: Collation::Binary,
                nulls_first: false,
            }],
        );
        insert_row(&mut vm, 0, &[Value::Integer(5)]);
        // Dispatched from exec.rs as `SorterSort | Sort => sorter_sort`;
        // this test exercises the same function `Sort` maps to directly.
        let step = sorter_sort(&mut vm, &Instruction::new(Opcode::Sort, 0, 999, 0)).unwrap();
        assert_eq!(step, Step::Next);
    }

    fn sorted_ints(nulls_first: bool, descending: bool) -> Vec<Value> {
        let mut vm = Vm::new();
        open_sorter(
            &mut vm,
            0,
            vec![SortKeyColumn {
                index: 0,
                descending,
                collation: Collation::Binary,
                nulls_first,
            }],
        );
        insert_row(&mut vm, 0, &[Value::Integer(5)]);
        insert_row(&mut vm, 0, &[Value::Null]);
        insert_row(&mut vm, 0, &[Value::Integer(-7)]);
        insert_row(&mut vm, 0, &[Value::Integer(0)]);
        insert_row(&mut vm, 0, &[Value::Integer(5)]);

        sorter_sort(&mut vm, &Instruction::new(Opcode::SorterSort, 0, 999, 0)).unwrap();

        let mut seen = Vec::new();
        loop {
            sorter_data(&mut vm, &Instruction::new(Opcode::SorterData, 0, 5, 0)).unwrap();
            let Value::Blob(bytes) = vm.register(5).unwrap() else {
                panic!("expected a Blob");
            };
            let row = decode_record(bytes, TextEncoding::Utf8).unwrap();
            seen.push(row[0].clone());
            match sorter_next(&mut vm, &Instruction::new(Opcode::SorterNext, 0, 1, 0)).unwrap() {
                Step::Jump(1) => continue,
                Step::Next => break,
                other => panic!("unexpected step {other:?}"),
            }
        }
        seen
    }

    #[test]
    fn ascending_default_places_nulls_first() {
        assert_eq!(
            sorted_ints(true, false),
            vec![
                Value::Null,
                Value::Integer(-7),
                Value::Integer(0),
                Value::Integer(5),
                Value::Integer(5),
            ]
        );
    }

    #[test]
    fn descending_default_places_nulls_last() {
        assert_eq!(
            sorted_ints(false, true),
            vec![
                Value::Integer(5),
                Value::Integer(5),
                Value::Integer(0),
                Value::Integer(-7),
                Value::Null,
            ]
        );
    }

    #[test]
    fn ascending_with_nulls_last_matches_oracle() {
        // SELECT i FROM t ORDER BY i ASC NULLS LAST (issue #140)
        assert_eq!(
            sorted_ints(false, false),
            vec![
                Value::Integer(-7),
                Value::Integer(0),
                Value::Integer(5),
                Value::Integer(5),
                Value::Null,
            ]
        );
    }

    #[test]
    fn descending_with_nulls_first_matches_oracle() {
        // SELECT i FROM t ORDER BY i DESC NULLS FIRST (issue #140)
        assert_eq!(
            sorted_ints(true, true),
            vec![
                Value::Null,
                Value::Integer(5),
                Value::Integer(5),
                Value::Integer(0),
                Value::Integer(-7),
            ]
        );
    }
}
