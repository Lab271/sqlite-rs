//! Arithmetic opcodes (spec 009, Requirement 6): `Add`, `Subtract`,
//! `Multiply`, `Divide`, `Remainder`. Each reads two source registers
//! and writes one destination register — all overflow, NULL-propagation,
//! and numeric-coercion behavior is spec 008's, via `src/vdbe/coerce.rs`
//! and `src/vdbe/value.rs`. No arithmetic happens in this file.

use crate::record::Value;
use crate::vdbe::coerce::{checked_add, checked_div, checked_mul, checked_rem, checked_sub};
use crate::vdbe::exec::{ExecError, Step, Vm};
use crate::vdbe::program::Instruction;

/// NULL propagates through arithmetic (spec 008, Requirement 4): any
/// NULL operand makes the result NULL, checked before delegating to the
/// kernel's checked ops (which otherwise treat NULL as numeric 0).
fn binary_op(
    vm: &mut Vm,
    instr: &Instruction,
    op: fn(&Value, &Value) -> Value,
) -> Result<Step, ExecError> {
    let a = vm.register(instr.p1)?.clone();
    let b = vm.register(instr.p2)?.clone();
    let result = if matches!(a, Value::Null) || matches!(b, Value::Null) {
        Value::Null
    } else {
        op(&a, &b)
    };
    vm.set_register(instr.p3, result)?;
    Ok(Step::Next)
}

/// `Add`: `r[P3] = r[P1] + r[P2]`.
pub fn add(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    binary_op(vm, instr, checked_add)
}

/// `Subtract`: `r[P3] = r[P2] - r[P1]`, matching SQLite's operand order
/// (the harvested `Subtract` computes P2 minus P1, not P1 minus P2).
pub fn subtract(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let a = vm.register(instr.p1)?.clone();
    let b = vm.register(instr.p2)?.clone();
    let result = if matches!(a, Value::Null) || matches!(b, Value::Null) {
        Value::Null
    } else {
        checked_sub(&b, &a)
    };
    vm.set_register(instr.p3, result)?;
    Ok(Step::Next)
}

/// `Multiply`: `r[P3] = r[P1] * r[P2]`.
pub fn multiply(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    binary_op(vm, instr, checked_mul)
}

/// `Divide`: `r[P3] = r[P2] / r[P1]`, matching SQLite's operand order.
pub fn divide(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let a = vm.register(instr.p1)?.clone();
    let b = vm.register(instr.p2)?.clone();
    let result = if matches!(a, Value::Null) || matches!(b, Value::Null) {
        Value::Null
    } else {
        checked_div(&b, &a)
    };
    vm.set_register(instr.p3, result)?;
    Ok(Step::Next)
}

/// `Remainder`: `r[P3] = r[P2] % r[P1]`, matching SQLite's operand order.
pub fn remainder(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let a = vm.register(instr.p1)?.clone();
    let b = vm.register(instr.p2)?.clone();
    let result = if matches!(a, Value::Null) || matches!(b, Value::Null) {
        Value::Null
    } else {
        checked_rem(&b, &a)
    };
    vm.set_register(instr.p3, result)?;
    Ok(Step::Next)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::vdbe::program::Opcode;

    fn vm_with(regs: Vec<Value>) -> Vm {
        let mut vm = Vm::new();
        for (i, v) in regs.into_iter().enumerate() {
            vm.set_register(i as i32, v).unwrap();
        }
        vm
    }

    #[test]
    fn add_reads_two_registers_writes_one() {
        let mut vm = vm_with(vec![Value::Integer(1), Value::Integer(2)]);
        let instr = Instruction::new(Opcode::Add, 0, 1, 2);
        add(&mut vm, &instr).unwrap();
        assert_eq!(*vm.register(2).unwrap(), Value::Integer(3));
    }

    #[test]
    fn null_propagates_through_every_arithmetic_opcode() {
        for op in [add, subtract, multiply, divide, remainder] {
            let mut vm = vm_with(vec![Value::Null, Value::Integer(2)]);
            let instr = Instruction::new(Opcode::Add, 0, 1, 2);
            op(&mut vm, &instr).unwrap();
            assert_eq!(*vm.register(2).unwrap(), Value::Null);
        }
    }

    #[test]
    fn divide_by_zero_yields_null_not_a_panic() {
        let mut vm = vm_with(vec![Value::Integer(0), Value::Integer(10)]);
        let instr = Instruction::new(Opcode::Divide, 0, 1, 2);
        divide(&mut vm, &instr).unwrap();
        assert_eq!(*vm.register(2).unwrap(), Value::Null);
    }

    #[test]
    fn integer_overflow_promotes_to_real() {
        let mut vm = vm_with(vec![Value::Integer(i64::MAX), Value::Integer(1)]);
        let instr = Instruction::new(Opcode::Add, 0, 1, 2);
        add(&mut vm, &instr).unwrap();
        assert!(matches!(vm.register(2).unwrap(), Value::Real(_)));
    }
}
