//! Control-flow opcodes (spec 009, Requirement 3): unconditional jump,
//! one-shot guarding, subroutine call/return, NULL-testing conditional
//! jumps, and the LIMIT/OFFSET counter family. Pure control flow — no
//! value semantics of its own beyond the truthiness/integer-convertible
//! checks these opcodes are defined by.

use crate::record::Value;
use crate::vdbe::exec::{ExecError, Step, Vm};
use crate::vdbe::program::Instruction;

/// `Init`: jumps to `P2` (the start of the main program body) unless
/// `P2` is 0, in which case execution falls through to PC+1.
pub fn init(instr: &Instruction) -> Result<Step, ExecError> {
    Ok(jump_or_next(instr.p2))
}

/// `Goto`: unconditional jump to `P2`.
pub fn goto(instr: &Instruction) -> Result<Step, ExecError> {
    Ok(Step::Jump(to_pc(instr.p2)))
}

/// `Once`: runs its guarded block the first time control reaches this
/// instruction address, then jumps to `P2` on every subsequent visit.
pub fn once(vm: &mut Vm, pc: usize, instr: &Instruction) -> Result<Step, ExecError> {
    if vm.once_fired.insert(pc) {
        Ok(Step::Next)
    } else {
        Ok(Step::Jump(to_pc(instr.p2)))
    }
}

/// `BeginSubrtn`: marks a subroutine's entry point; falls straight
/// through. The call site is responsible for storing a return address
/// and jumping here.
pub fn begin_subrtn() -> Result<Step, ExecError> {
    Ok(Step::Next)
}

/// `Return`: jumps to the address stored (as an integer) in register
/// `P1`.
pub fn r#return(vm: &Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let addr = vm.register(instr.p1)?;
    match addr {
        Value::Integer(i) => match i32::try_from(*i) {
            Ok(pc) => Ok(Step::Jump(to_pc(pc))),
            Err(_) => Err(ExecError::MalformedInstruction {
                opcode: "Return",
                reason: format!("return address {i} does not fit in a PC"),
            }),
        },
        other => Err(ExecError::TypeMismatch {
            opcode: "Return",
            found: value_kind(other),
        }),
    }
}

/// `Halt`: terminates execution. `P1` is the SQLite result code (0 =
/// success); `P4`, if present, carries an error message.
pub fn halt(instr: &Instruction) -> Result<Step, ExecError> {
    let message = match &instr.p4 {
        crate::vdbe::program::P4::Str(s) => Some(s.clone()),
        _ => None,
    };
    Ok(Step::Halt {
        code: instr.p1,
        message,
    })
}

/// `Transaction`: a no-op in this VM — transaction/schema-cookie
/// machinery lives outside the bytecode interpreter (V1's pager).
pub fn transaction() -> Result<Step, ExecError> {
    Ok(Step::Next)
}

/// `IsNull`: jumps to `P2` if register `P1` holds NULL.
pub fn is_null(vm: &Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let v = vm.register(instr.p1)?;
    Ok(if matches!(v, Value::Null) {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `NotNull`: jumps to `P2` if register `P1` does not hold NULL.
pub fn not_null(vm: &Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let v = vm.register(instr.p1)?;
    Ok(if matches!(v, Value::Null) {
        Step::Next
    } else {
        Step::Jump(to_pc(instr.p2))
    })
}

/// `IfNot`: jumps to `P2` if register `P1` is falsy (zero). NULL jumps
/// only when `P3` is nonzero.
pub fn if_not(vm: &Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let v = vm.register(instr.p1)?;
    let take_jump = match v {
        Value::Null => instr.p3 != 0,
        other => is_falsy(other),
    };
    Ok(if take_jump {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// Truthiness for the boolean-consuming opcodes (`IfNot` here,
/// `Not` in `arithmetic.rs`): SQLite's `sqlite3VdbeBooleanValue`, i.e.
/// numeric-coerced zero is false and everything else is true. NULL is
/// neither — callers decide what to do with it, which is why this
/// answers `false` for NULL rather than pretending it is a boolean.
pub(crate) fn is_falsy(v: &Value) -> bool {
    match v {
        Value::Integer(i) => *i == 0,
        Value::Real(r) => *r == 0.0,
        Value::Null => false,
        Value::Text(s) => match crate::vdbe::coerce_text_to_numeric(s) {
            Value::Integer(i) => i == 0,
            Value::Real(r) => r == 0.0,
            _ => true,
        },
        Value::Blob(_) => true,
    }
}

/// `MustBeInt`: forces register `P1` to be an integer, converting a
/// losslessly-integral REAL or well-formed integer-text in place. If the
/// value cannot be converted without data loss, jumps to `P2` (or, when
/// `P2` is 0, reports an error).
pub fn must_be_int(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let v = vm.register(instr.p1)?.clone();
    match try_to_integer(&v) {
        Some(i) => {
            vm.set_register(instr.p1, Value::Integer(i))?;
            Ok(Step::Next)
        }
        None if instr.p2 != 0 => Ok(Step::Jump(to_pc(instr.p2))),
        None => Err(ExecError::MustBeInt),
    }
}

fn try_to_integer(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(i) => Some(*i),
        #[allow(clippy::cast_possible_truncation)]
        Value::Real(r) if r.fract() == 0.0 && r.is_finite() && in_i64_range(*r) => Some(*r as i64),
        Value::Text(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// Whether `r` falls within `i64`'s representable range — guards the
/// `as i64` cast above, which otherwise saturates (rather than erroring)
/// on an integral-but-out-of-range REAL like `1e300`.
fn in_i64_range(r: f64) -> bool {
    r >= i64::MIN as f64 && r < i64::MAX as f64
}

/// `OffsetLimit`: computes a combined row-budget counter from a LIMIT
/// register (`P1`) and an OFFSET register (`P3`) into destination
/// register `P2`. A non-positive LIMIT means "no limit", encoded as -1.
pub fn offset_limit(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let limit = register_as_i64(vm, instr.p1)?;
    let offset = register_as_i64(vm, instr.p3)?;
    let combined = if limit > 0 {
        limit.saturating_add(offset.max(0))
    } else {
        -1
    };
    vm.set_register(instr.p2, Value::Integer(combined))?;
    Ok(Step::Next)
}

/// `IfPos`: if register `P1` is 1 or greater, subtracts `P3` from it and
/// jumps to `P2`.
pub fn if_pos(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let v = register_as_i64(vm, instr.p1)?;
    if v > 0 {
        vm.set_register(
            instr.p1,
            Value::Integer(v.saturating_sub(i64::from(instr.p3))),
        )?;
        Ok(Step::Jump(to_pc(instr.p2)))
    } else {
        Ok(Step::Next)
    }
}

/// `IfNotZero`: if register `P1` is nonzero, decrements it (when
/// positive) and jumps to `P2`.
pub fn if_not_zero(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let v = register_as_i64(vm, instr.p1)?;
    if v != 0 {
        if v > 0 {
            vm.set_register(instr.p1, Value::Integer(v.saturating_sub(1)))?;
        }
        Ok(Step::Jump(to_pc(instr.p2)))
    } else {
        Ok(Step::Next)
    }
}

/// `DecrJumpZero`: decrements register `P1`, then jumps to `P2` if the
/// new value is exactly zero.
pub fn decr_jump_zero(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let v = register_as_i64(vm, instr.p1)?.saturating_sub(1);
    vm.set_register(instr.p1, Value::Integer(v))?;
    Ok(if v == 0 {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

fn register_as_i64(vm: &Vm, reg: i32) -> Result<i64, ExecError> {
    match vm.register(reg)? {
        Value::Integer(i) => Ok(*i),
        other => Err(ExecError::TypeMismatch {
            opcode: "control counter",
            found: value_kind(other),
        }),
    }
}

fn jump_or_next(p2: i32) -> Step {
    if p2 == 0 {
        Step::Next
    } else {
        Step::Jump(to_pc(p2))
    }
}

#[allow(clippy::cast_sign_loss)]
fn to_pc(p2: i32) -> usize {
    p2.max(0) as usize
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "NULL",
        Value::Integer(_) => "INTEGER",
        Value::Real(_) => "REAL",
        Value::Text(_) => "TEXT",
        Value::Blob(_) => "BLOB",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::vdbe::program::Opcode;
    use crate::vdbe::Vm as VmType;

    fn vm_with(regs: Vec<Value>) -> VmType {
        let mut vm = VmType::new();
        for (i, v) in regs.into_iter().enumerate() {
            vm.set_register(i as i32, v).unwrap();
        }
        vm
    }

    #[test]
    fn decr_jump_zero_terminates_at_zero() {
        let mut vm = vm_with(vec![Value::Integer(1)]);
        let instr = Instruction::new(Opcode::DecrJumpZero, 0, 10, 0);
        assert_eq!(decr_jump_zero(&mut vm, &instr).unwrap(), Step::Jump(10));
        assert_eq!(*vm.register(0).unwrap(), Value::Integer(0));
    }

    #[test]
    fn offset_limit_combines_limit_and_offset() {
        let mut vm = vm_with(vec![Value::Integer(2), Value::Null, Value::Integer(1)]);
        let instr = Instruction::new(Opcode::OffsetLimit, 0, 1, 2);
        offset_limit(&mut vm, &instr).unwrap();
        assert_eq!(*vm.register(1).unwrap(), Value::Integer(3));
    }

    #[test]
    fn offset_limit_non_positive_limit_means_unbounded() {
        let mut vm = vm_with(vec![Value::Integer(0), Value::Null, Value::Integer(0)]);
        let instr = Instruction::new(Opcode::OffsetLimit, 0, 1, 2);
        offset_limit(&mut vm, &instr).unwrap();
        assert_eq!(*vm.register(1).unwrap(), Value::Integer(-1));
    }

    #[test]
    fn once_falls_through_first_time_then_jumps_on_repeat_entry() {
        let mut vm = VmType::new();
        let instr = Instruction::new(Opcode::Once, 0, 10, 0);
        assert_eq!(once(&mut vm, 3, &instr).unwrap(), Step::Next);
        assert_eq!(once(&mut vm, 3, &instr).unwrap(), Step::Jump(10));
        assert_eq!(once(&mut vm, 3, &instr).unwrap(), Step::Jump(10));
    }

    #[test]
    fn must_be_int_rejects_out_of_range_integral_real() {
        let mut vm = vm_with(vec![Value::Real(1e300)]);
        let instr = Instruction::new(Opcode::MustBeInt, 0, 0, 0);
        assert!(matches!(
            must_be_int(&mut vm, &instr),
            Err(ExecError::MustBeInt)
        ));
    }

    #[test]
    fn return_rejects_address_that_does_not_fit_in_a_pc() {
        let vm = vm_with(vec![Value::Integer(i64::MAX)]);
        let instr = Instruction::new(Opcode::Return, 0, 0, 0);
        assert!(matches!(
            r#return(&vm, &instr),
            Err(ExecError::MalformedInstruction {
                opcode: "Return",
                ..
            })
        ));
    }

    #[test]
    fn is_null_and_not_null_jump_on_null() {
        let vm = vm_with(vec![Value::Null, Value::Integer(1)]);
        let instr = Instruction::new(Opcode::IsNull, 0, 5, 0);
        assert_eq!(is_null(&vm, &instr).unwrap(), Step::Jump(5));
        let instr = Instruction::new(Opcode::NotNull, 1, 5, 0);
        assert_eq!(not_null(&vm, &instr).unwrap(), Step::Jump(5));
    }
}
