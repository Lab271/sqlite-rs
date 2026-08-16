//! The register file, cursor-slot table, and fetch-decode-execute loop
//! (spec 009, Requirement 2), plus dispatch for the compare opcodes
//! (Requirement 5) — the only opcode family whose delegation to the
//! kernel (`src/vdbe/compare.rs`, `src/vdbe/affinity.rs`) is thin enough
//! to live directly in the dispatcher rather than its own file.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::rc::Rc;

use thiserror::Error;

use crate::header::DatabaseHeader;
use crate::record::Value;
use crate::vdbe::affinity::{apply_affinity, Affinity};
use crate::vdbe::collation::Collation;
use crate::vdbe::compare::compare;
use crate::vdbe::cursor::CursorSlot;
use crate::vdbe::program::{Instruction, Opcode, Program, P4};
use crate::vdbe::{arithmetic, control, cursor, result, sorter};
use crate::vfs::PageSource;

#[derive(Debug, Error)]
pub enum ExecError {
    #[error("{opcode}: register index {index} is out of range")]
    RegisterOutOfRange { opcode: &'static str, index: i32 },

    #[error("{opcode}: register range count {count} exceeds the maximum ({MAX_REGISTERS})")]
    RegisterRangeTooLarge { opcode: &'static str, count: i32 },

    #[error("{opcode}: expected a different value type, found {found}")]
    TypeMismatch {
        opcode: &'static str,
        found: &'static str,
    },

    #[error("MustBeInt: value cannot be converted to an integer without data loss")]
    MustBeInt,

    #[error("{opcode}: malformed instruction ({reason})")]
    MalformedInstruction {
        opcode: &'static str,
        reason: String,
    },

    #[error("opcode {opcode:?} is not yet implemented by this VM")]
    Unimplemented { opcode: Opcode },

    #[error("cursor slot {slot} is not open")]
    CursorNotOpen { slot: i32 },

    #[error("{opcode}: cursor slot {slot} is a {found}, not a {expected}")]
    CursorTypeMismatch {
        opcode: &'static str,
        slot: i32,
        found: &'static str,
        expected: &'static str,
    },

    #[error("{opcode} requires a database attached to this VM (see Vm::with_db)")]
    NoDatabase { opcode: &'static str },

    #[error("program counter {pc} is out of range")]
    ProgramCounterOutOfRange { pc: usize },

    #[error("program exceeded the maximum step count ({MAX_STEPS}) without halting")]
    StepLimitExceeded,

    #[error("statement halted with SQLite result code {code}{}", message.as_deref().map(|m| format!(": {m}")).unwrap_or_default())]
    Halted { code: i32, message: Option<String> },
}

/// The outcome of executing one instruction: fall through to PC+1, jump
/// to an explicit target, or halt the program.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Next,
    Jump(usize),
    Halt { code: i32, message: Option<String> },
}

/// The database a `Vm` reads real table cursors from (`OpenRead`) — the
/// page source plus the header fields (usable page size) `TableCursor`
/// needs. Absent for programs that never open a real cursor (e.g. every
/// #89 arithmetic/control test, and this ticket's sorter/ephemeral-only
/// tests) — those construct a `Vm` via `Vm::new()` and never hit
/// `OpenRead`.
#[derive(Clone)]
pub(crate) struct VmDb {
    pub(crate) source: Rc<dyn PageSource>,
    pub(crate) header: DatabaseHeader,
}

impl std::fmt::Debug for VmDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VmDb")
            .field("header", &self.header)
            .finish_non_exhaustive()
    }
}

/// The VM's mutable execution state: a register file of `Value` cells
/// and a disjoint cursor-slot table (Requirement 2), plus the
/// accumulated output rows and the one-shot-guard bookkeeping `Once`
/// needs.
#[derive(Debug, Default)]
pub struct Vm {
    registers: Vec<Value>,
    /// Cursor-slot storage: a disjoint address space from `registers`,
    /// so a cursor slot and a register of the same integer index never
    /// alias (Requirement 2). `None` until the slot's `Open*` opcode
    /// runs; each open slot holds one of [`CursorSlot`]'s variants (a
    /// real table cursor, an in-memory ephemeral index, a sorter, or a
    /// single-row pseudo-cursor).
    cursors: Vec<Option<CursorSlot>>,
    pub(crate) db: Option<VmDb>,
    rows: Vec<Vec<Value>>,
    pub(crate) once_fired: HashSet<usize>,
}

/// Caps a single register index and, separately, a register-range
/// *count* (`MakeRecord`/`ResultRow`'s `P2`) — a backstop against an
/// adversarial or corrupt instruction whose oversized operand would
/// otherwise drive a multi-gigabyte allocation (`Vec::with_capacity`,
/// register-file `resize`) well before any legitimate program would ever
/// need this many registers.
pub(crate) const MAX_REGISTERS: usize = 1 << 20;

impl Vm {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a `Vm` that can service `OpenRead` against `source` (page
    /// size / usable size taken from `header`). Every `OpenRead` in the
    /// program shares this one `source` via cheap `Rc` clones, so
    /// multiple open cursors never contend over exclusive ownership of
    /// the underlying file handle.
    pub fn with_db(source: Rc<dyn PageSource>, header: DatabaseHeader) -> Self {
        Self {
            db: Some(VmDb { source, header }),
            ..Self::default()
        }
    }

    pub(crate) fn db(&self) -> Result<&VmDb, ExecError> {
        self.db
            .as_ref()
            .ok_or(ExecError::NoDatabase { opcode: "OpenRead" })
    }

    #[allow(clippy::cast_sign_loss)]
    fn index(opcode: &'static str, reg: i32) -> Result<usize, ExecError> {
        if reg < 0 || reg as usize > MAX_REGISTERS {
            return Err(ExecError::RegisterOutOfRange { opcode, index: reg });
        }
        Ok(reg as usize)
    }

    /// Validates a `[p1, p1+count)` register range's *count* operand
    /// (before any allocation sized by it), returning `count` as a
    /// `usize` on success.
    pub(crate) fn bounded_count(opcode: &'static str, count: i32) -> Result<usize, ExecError> {
        if !(0..=MAX_REGISTERS as i32).contains(&count) {
            return Err(ExecError::RegisterRangeTooLarge { opcode, count });
        }
        #[allow(clippy::cast_sign_loss)]
        Ok(count as usize)
    }

    /// Reads register `reg`. Registers not yet written read as NULL —
    /// a register file has no implicit-clearing surprises to guard
    /// against (Requirement 2), only never-written cells.
    pub fn register(&self, reg: i32) -> Result<&Value, ExecError> {
        let idx = Self::index("register read", reg)?;
        Ok(self.registers.get(idx).unwrap_or(&Value::Null))
    }

    /// Writes register `reg`, growing the register file with NULL
    /// filler as needed.
    pub fn set_register(&mut self, reg: i32, value: Value) -> Result<(), ExecError> {
        let idx = Self::index("register write", reg)?;
        if idx >= self.registers.len() {
            self.registers.resize(idx.saturating_add(1), Value::Null);
        }
        if let Some(slot) = self.registers.get_mut(idx) {
            *slot = value;
        }
        Ok(())
    }

    /// Reads cursor slot `slot`. Occupies a namespace disjoint from
    /// `register` — the same integer index in each never aliases.
    /// Errors if the slot has no cursor open on it (no `Open*` opcode
    /// has run for this slot, or it was never a valid index).
    pub(crate) fn cursor(&self, slot: i32) -> Result<&CursorSlot, ExecError> {
        let idx = Self::index("cursor slot read", slot)?;
        self.cursors
            .get(idx)
            .and_then(Option::as_ref)
            .ok_or(ExecError::CursorNotOpen { slot })
    }

    /// Mutable counterpart to [`Self::cursor`].
    pub(crate) fn cursor_mut(&mut self, slot: i32) -> Result<&mut CursorSlot, ExecError> {
        let idx = Self::index("cursor slot write", slot)?;
        self.cursors
            .get_mut(idx)
            .and_then(Option::as_mut)
            .ok_or(ExecError::CursorNotOpen { slot })
    }

    /// Opens cursor slot `slot` with `value`, growing the cursor-slot
    /// table with empty (unopened) filler as needed — mirrors
    /// `set_register`'s growth policy, but into the disjoint `cursors`
    /// storage. Overwrites (closes) any cursor already open on `slot`.
    pub(crate) fn set_cursor(&mut self, slot: i32, value: CursorSlot) -> Result<(), ExecError> {
        let idx = Self::index("cursor slot write", slot)?;
        if idx >= self.cursors.len() {
            self.cursors.resize_with(idx.saturating_add(1), || None);
        }
        if let Some(cell) = self.cursors.get_mut(idx) {
            *cell = Some(value);
        }
        Ok(())
    }

    pub fn emit_row(&mut self, row: Vec<Value>) {
        self.rows.push(row);
    }

    pub fn rows(&self) -> &[Vec<Value>] {
        &self.rows
    }
}

#[allow(clippy::cast_sign_loss)]
pub(crate) fn to_pc(p2: i32) -> usize {
    p2.max(0) as usize
}

/// Compare opcodes (`Eq`/`Ge`/`Gt`/`Le`/`Lt`): jump to `P2` if `r[P1]
/// <op> r[P3]` holds, per the kernel's cross-type comparison order.
/// Either operand NULL means the comparison is unknown, so no jump is
/// taken — matching SQL's three-valued-logic default (no `SQLITE_NULLEQ`
/// support in this VM). `P4`, if a `CollSeq` descriptor, selects the
/// collation for text-vs-text comparisons; absent P4 defaults to BINARY.
fn compare_jump(
    vm: &Vm,
    instr: &Instruction,
    holds: fn(Ordering) -> bool,
) -> Result<Step, ExecError> {
    let a = vm.register(instr.p1)?;
    let b = vm.register(instr.p3)?;
    if matches!(a, Value::Null) || matches!(b, Value::Null) {
        return Ok(Step::Next);
    }
    let collation = match &instr.p4 {
        P4::CollSeq { collation, .. } => *collation,
        _ => Collation::Binary,
    };
    let ord = compare(a, b, collation);
    Ok(if holds(ord) {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `RealAffinity`: applies REAL affinity coercion to register `P1` in
/// place, delegating to the kernel's affinity rules — independent of
/// any comparison, per spec 009 Requirement 5.
fn real_affinity(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let mut v = vm.register(instr.p1)?.clone();
    apply_affinity(&mut v, Affinity::Real);
    vm.set_register(instr.p1, v)?;
    Ok(Step::Next)
}

fn dispatch(vm: &mut Vm, pc: usize, instr: &Instruction) -> Result<Step, ExecError> {
    use Opcode::{
        Add, BeginSubrtn, Column, DecrJumpZero, Delete, Divide, Eq, Found, Function, Ge, Goto, Gt,
        Halt, IdxInsert, IdxLE, IfNot, IfNotZero, IfPos, Init, Integer, IsNull, Last, Le, Lt,
        MakeRecord, Multiply, MustBeInt, Next, NotNull, NullRow, OffsetLimit, Once, OpenEphemeral,
        OpenPseudo, OpenRead, RealAffinity, Remainder, ResultRow, Return, Rewind, Rowid, SeekRowid,
        Sequence, Sort, SorterData, SorterInsert, SorterNext, SorterOpen, SorterSort, String8,
        Subtract, Transaction,
    };
    match instr.opcode {
        Init => control::init(instr),
        Goto => control::goto(instr),
        Once => control::once(vm, pc, instr),
        BeginSubrtn => control::begin_subrtn(),
        Return => control::r#return(vm, instr),
        Halt => control::halt(instr),
        Transaction => control::transaction(),
        IfNot => control::if_not(vm, instr),
        IfNotZero => control::if_not_zero(vm, instr),
        IfPos => control::if_pos(vm, instr),
        DecrJumpZero => control::decr_jump_zero(vm, instr),
        IsNull => control::is_null(vm, instr),
        NotNull => control::not_null(vm, instr),
        MustBeInt => control::must_be_int(vm, instr),
        OffsetLimit => control::offset_limit(vm, instr),

        Eq => compare_jump(vm, instr, |o| o == Ordering::Equal),
        Ge => compare_jump(vm, instr, |o| o != Ordering::Less),
        Gt => compare_jump(vm, instr, |o| o == Ordering::Greater),
        Le => compare_jump(vm, instr, |o| o != Ordering::Greater),
        Lt => compare_jump(vm, instr, |o| o == Ordering::Less),
        RealAffinity => real_affinity(vm, instr),

        Add => arithmetic::add(vm, instr),
        Subtract => arithmetic::subtract(vm, instr),
        Multiply => arithmetic::multiply(vm, instr),
        Divide => arithmetic::divide(vm, instr),
        Remainder => arithmetic::remainder(vm, instr),

        Integer => result::integer(vm, instr),
        String8 => result::string8(vm, instr),
        MakeRecord => result::make_record(vm, instr),
        ResultRow => result::result_row(vm, instr),

        OpenRead => cursor::open_read(vm, instr),
        OpenEphemeral => cursor::open_ephemeral(vm, instr),
        OpenPseudo => cursor::open_pseudo(vm, instr),
        Rewind => cursor::rewind(vm, instr),
        Last => cursor::last(vm, instr),
        Next => cursor::next(vm, instr),
        Column => cursor::column(vm, instr),
        Rowid => cursor::rowid(vm, instr),
        SeekRowid => cursor::seek_rowid(vm, instr),
        NullRow => cursor::null_row(vm, instr),
        Sequence => cursor::sequence(vm, instr),
        Found => cursor::found(vm, instr),
        IdxInsert => cursor::idx_insert(vm, instr),
        IdxLE => cursor::idx_le(vm, instr),
        Delete => cursor::delete(vm, instr),

        SorterOpen => sorter::sorter_open(vm, instr),
        SorterInsert => sorter::sorter_insert(vm, instr),
        SorterSort | Sort => sorter::sorter_sort(vm, instr),
        SorterNext => sorter::sorter_next(vm, instr),
        SorterData => sorter::sorter_data(vm, instr),

        Function => function(vm, instr),
    }
}

/// `Function` (spec 009, Requirement 7): dispatches by a `P4`
/// `"name(arity)"` descriptor into the shared scalar-function registry
/// (`src/vdbe/functions.rs`, spec 008), reading its argument registers
/// (a contiguous run starting at `P2`) and writing the result to `P3`.
/// No function-specific logic lives here — adding a function to the
/// registry is sufficient to make it callable via this opcode.
fn function(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let descriptor = match &instr.p4 {
        P4::Str(s) => s.as_str(),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "Function",
                reason: format!("expected a \"name(arity)\" string P4, got {other:?}"),
            })
        }
    };
    let (name, arity) =
        parse_function_descriptor(descriptor).ok_or_else(|| ExecError::MalformedInstruction {
            opcode: "Function",
            reason: format!("malformed function descriptor {descriptor:?}"),
        })?;
    let mut args = Vec::with_capacity(arity);
    for i in 0..arity {
        let reg = instr
            .p2
            .checked_add(
                i32::try_from(i).map_err(|_| ExecError::RegisterRangeTooLarge {
                    opcode: "Function",
                    count: i32::try_from(arity).unwrap_or(i32::MAX),
                })?,
            )
            .ok_or(ExecError::RegisterOutOfRange {
                opcode: "Function",
                index: instr.p2,
            })?;
        args.push(vm.register(reg)?.clone());
    }
    let result =
        crate::vdbe::functions::call(name, &args).map_err(|e| ExecError::MalformedInstruction {
            opcode: "Function",
            reason: e.to_string(),
        })?;
    vm.set_register(instr.p3, result)?;
    Ok(Step::Next)
}

/// Parses a `"name(arity)"` descriptor (e.g. `"abs(1)"`, `"like(2)"`)
/// into its parts.
fn parse_function_descriptor(descriptor: &str) -> Option<(&str, usize)> {
    let open = descriptor.find('(')?;
    if !descriptor.ends_with(')') {
        return None;
    }
    let name = descriptor.get(..open)?;
    let inner_start = open.checked_add(1)?;
    let inner_end = descriptor.len().checked_sub(1)?;
    let arity: usize = descriptor.get(inner_start..inner_end)?.parse().ok()?;
    Some((name, arity))
}

/// Runs `program` to completion, starting at PC 0. Returns the rows
/// emitted via `ResultRow`, or an error — including a non-zero `Halt`,
/// surfaced as `ExecError::Halted` rather than silently discarded. Never
/// panics: any malformed instruction (out-of-range register, wrong P4
/// type, PC running past the end of the program without a `Halt`)
/// returns a structured `Err`.
/// Caps total instructions executed per run — a backstop against a
/// malformed or adversarial program looping forever (e.g. a bare `Goto`
/// cycle), not a performance tuning knob. Well past any real program's
/// legitimate instruction count.
const MAX_STEPS: u32 = 1_000_000;

pub fn execute(program: &Program) -> Result<Vec<Vec<Value>>, ExecError> {
    run(Vm::new(), program)
}

/// Like [`execute`], but the `Vm` can service `OpenRead` (cursor
/// opcodes over real tables) against `source`/`header` — see
/// [`Vm::with_db`].
pub fn execute_with_db(
    program: &Program,
    source: Rc<dyn PageSource>,
    header: DatabaseHeader,
) -> Result<Vec<Vec<Value>>, ExecError> {
    run(Vm::with_db(source, header), program)
}

fn run(mut vm: Vm, program: &Program) -> Result<Vec<Vec<Value>>, ExecError> {
    let mut pc = 0usize;
    let mut steps = 0u32;
    loop {
        steps = steps.checked_add(1).ok_or(ExecError::StepLimitExceeded)?;
        if steps > MAX_STEPS {
            return Err(ExecError::StepLimitExceeded);
        }
        let instr = program
            .get(pc)
            .ok_or(ExecError::ProgramCounterOutOfRange { pc })?;
        match dispatch(&mut vm, pc, instr)? {
            Step::Next => {
                pc = pc
                    .checked_add(1)
                    .ok_or(ExecError::ProgramCounterOutOfRange { pc })?;
            }
            Step::Jump(target) => pc = target,
            Step::Halt { code: 0, .. } => return Ok(vm.rows),
            Step::Halt { code, message } => return Err(ExecError::Halted { code, message }),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::vdbe::program::Instruction;

    #[test]
    fn register_persists_across_unrelated_instructions() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(42)).unwrap();
        vm.set_register(1, Value::Integer(0)).unwrap();
        assert_eq!(*vm.register(0).unwrap(), Value::Integer(42));
    }

    #[test]
    fn cursor_slots_and_registers_are_disjoint() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(1)).unwrap();
        vm.set_cursor(0, CursorSlot::Pseudo { register: 99 })
            .unwrap();
        assert_eq!(*vm.register(0).unwrap(), Value::Integer(1));
        assert!(matches!(
            vm.cursor(0).unwrap(),
            CursorSlot::Pseudo { register: 99 }
        ));
    }

    #[test]
    fn program_falls_off_the_end_without_halt_errors_not_panics() {
        // A single non-jumping instruction with no `Halt` afterward runs
        // off the end of the program — must error, never panic.
        let program = Program::new(vec![Instruction::new(Opcode::Integer, 1, 0, 0)]);
        assert!(matches!(
            execute(&program),
            Err(ExecError::ProgramCounterOutOfRange { .. })
        ));
    }

    #[test]
    fn hand_assembled_program_computes_1_plus_2_and_emits_a_row() {
        // Integer 1 -> r0; Integer 2 -> r1; Add r0,r1 -> r2; ResultRow r2,1; Halt
        let program = Program::new(vec![
            Instruction::new(crate::vdbe::program::Opcode::Integer, 1, 0, 0),
            Instruction::new(crate::vdbe::program::Opcode::Integer, 2, 1, 0),
            Instruction::new(Opcode::Add, 0, 1, 2),
            Instruction::new(Opcode::ResultRow, 2, 1, 0),
            Instruction::new(Opcode::Halt, 0, 0, 0),
        ]);
        let rows = execute(&program).unwrap();
        assert_eq!(rows, vec![vec![Value::Integer(3)]]);
    }

    #[test]
    fn compare_opcodes_jump_on_kernel_result_not_a_re_derived_rule() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(10)).unwrap();
        vm.set_register(1, Value::Integer(5)).unwrap();
        let ge = Instruction::new(Opcode::Ge, 0, 99, 1);
        assert_eq!(
            compare_jump(&vm, &ge, |o| o != Ordering::Less).unwrap(),
            Step::Jump(99)
        );

        let lt = Instruction::new(Opcode::Lt, 0, 99, 1);
        assert_eq!(
            compare_jump(&vm, &lt, |o| o == Ordering::Less).unwrap(),
            Step::Next
        );
    }

    #[test]
    fn real_affinity_coerces_register_on_load_independent_of_comparison() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Text("1.5".to_string())).unwrap();
        real_affinity(&mut vm, &Instruction::new(Opcode::RealAffinity, 0, 0, 0)).unwrap();
        assert_eq!(*vm.register(0).unwrap(), Value::Real(1.5));
    }

    #[test]
    fn oversized_register_index_errors_instead_of_allocating_gigabytes() {
        // Regression for a fuzzer-found OOM: a corrupt/adversarial
        // instruction's register operand must be bounds-checked before
        // it drives a register-file `resize`, not just accepted as any
        // non-negative `i32`.
        let program = Program::new(vec![Instruction::new(Opcode::Integer, 1, 1_500_000_000, 0)]);
        assert!(matches!(
            execute(&program),
            Err(ExecError::RegisterOutOfRange { .. })
        ));
    }

    #[test]
    fn oversized_result_row_count_errors_instead_of_allocating_gigabytes() {
        let program = Program::new(vec![Instruction::new(
            Opcode::ResultRow,
            0,
            1_500_000_000,
            0,
        )]);
        assert!(matches!(
            execute(&program),
            Err(ExecError::RegisterRangeTooLarge { .. })
        ));
    }

    #[test]
    fn nonzero_halt_surfaces_as_an_error_not_a_result() {
        let program = Program::new(vec![Instruction::new(Opcode::Halt, 1, 0, 0)]);
        assert!(matches!(
            execute(&program),
            Err(ExecError::Halted { code: 1, .. })
        ));
    }
}
