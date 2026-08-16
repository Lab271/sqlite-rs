//! Result-row opcodes (spec 009, Requirement 8): literal loading
//! (`Integer`, `Null`, `String8`), record serialization (`MakeRecord`, reusing
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

/// `Null`: writes NULL into the register range `P2..=P3` (just `P2`
/// when `P3` is 0 or below `P2`). The only opcode that puts a NULL
/// into a register on purpose — without it, codegen's only NULL source
/// was "a register nobody ever wrote", which cannot express a NULL
/// that has to overwrite a live value (a three-valued comparison
/// result, a CASE with no matching branch).
pub fn null(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let last = instr.p3.max(instr.p2);
    for reg in instr.p2..=last {
        vm.set_register(reg, Value::Null)?;
    }
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
    fn null_overwrites_a_live_register_and_spans_p2_to_p3() {
        let mut vm = Vm::new();
        for r in 0..3 {
            vm.set_register(r, Value::Integer(9)).unwrap();
        }
        // P3 = 0 means "just P2", not "the range 1..=0".
        null(&mut vm, &Instruction::new(Opcode::Null, 0, 1, 0)).unwrap();
        assert_eq!(*vm.register(0).unwrap(), Value::Integer(9));
        assert_eq!(*vm.register(1).unwrap(), Value::Null);
        assert_eq!(*vm.register(2).unwrap(), Value::Integer(9));

        null(&mut vm, &Instruction::new(Opcode::Null, 0, 0, 2)).unwrap();
        for r in 0..3 {
            assert_eq!(*vm.register(r).unwrap(), Value::Null);
        }
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
