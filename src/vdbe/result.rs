//! Result-row opcodes (spec 009, Requirement 8): literal loading
//! (`Integer`, `String8`), record serialization (`MakeRecord`, reusing
//! spec 003's on-disk record encoding byte-for-byte), and row emission
//! (`ResultRow`).

use crate::record::{encode_record, TextEncoding, Value};
use crate::vdbe::exec::{ExecError, Step, Vm};
use crate::vdbe::program::{Instruction, P4};

/// `Integer`: loads the `i64` constant `P1` into register `P2`.
pub fn integer(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    vm.set_register(instr.p2, Value::Integer(i64::from(instr.p1)))?;
    Ok(Step::Next)
}

/// `String8`: loads the UTF-8 string constant in `P4` into register
/// `P2`.
pub fn string8(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let s = match &instr.p4 {
        P4::Str(s) => s.clone(),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "String8",
                reason: format!("expected a string P4, got {other:?}"),
            })
        }
    };
    vm.set_register(instr.p2, Value::Text(s))?;
    Ok(Step::Next)
}

/// `MakeRecord`: packs the contiguous register range `[P1, P1+P2)` into
/// spec 003's record format, writing the encoded bytes (as a `Value::
/// Blob`) to register `P3`.
pub fn make_record(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = Vm::bounded_count("MakeRecord", instr.p2)?;
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        let reg = instr
            .p1
            .checked_add(i as i32)
            .ok_or(ExecError::RegisterOutOfRange {
                opcode: "MakeRecord",
                index: instr.p1,
            })?;
        values.push(vm.register(reg)?.clone());
    }
    let payload = encode_record(&values, TextEncoding::Utf8);
    vm.set_register(instr.p3, Value::Blob(payload))?;
    Ok(Step::Next)
}

/// `ResultRow`: yields the contiguous register range `[P1, P1+P2)` as
/// one output row to the statement's caller.
pub fn result_row(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = Vm::bounded_count("ResultRow", instr.p2)?;
    let mut row = Vec::with_capacity(count);
    for i in 0..count {
        let reg = instr
            .p1
            .checked_add(i as i32)
            .ok_or(ExecError::RegisterOutOfRange {
                opcode: "ResultRow",
                index: instr.p1,
            })?;
        row.push(vm.register(reg)?.clone());
    }
    vm.emit_row(row);
    Ok(Step::Next)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::vdbe::program::Opcode;

    #[test]
    fn integer_and_string8_load_literals() {
        let mut vm = Vm::new();
        integer(&mut vm, &Instruction::new(Opcode::Integer, 42, 0, 0)).unwrap();
        assert_eq!(*vm.register(0).unwrap(), Value::Integer(42));

        let instr = Instruction::with_p4(Opcode::String8, 0, 1, 0, P4::Str("hello".to_string()));
        string8(&mut vm, &instr).unwrap();
        assert_eq!(*vm.register(1).unwrap(), Value::Text("hello".to_string()));
    }

    #[test]
    fn result_row_emits_fixed_register_range() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(1)).unwrap();
        vm.set_register(1, Value::Integer(2)).unwrap();
        result_row(&mut vm, &Instruction::new(Opcode::ResultRow, 0, 2, 0)).unwrap();
        assert_eq!(vm.rows(), &[vec![Value::Integer(1), Value::Integer(2)]]);
    }

    #[test]
    fn make_record_output_matches_spec_003_encoding() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(42)).unwrap();
        vm.set_register(1, Value::Text("abc".to_string())).unwrap();
        make_record(&mut vm, &Instruction::new(Opcode::MakeRecord, 0, 2, 2)).unwrap();
        let Value::Blob(payload) = vm.register(2).unwrap() else {
            panic!("expected a Blob");
        };
        let decoded = crate::record::decode_record(payload, TextEncoding::Utf8).unwrap();
        assert_eq!(
            decoded,
            vec![Value::Integer(42), Value::Text("abc".to_string())]
        );
    }
}
