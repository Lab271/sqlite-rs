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
use crate::vdbe::aggregate::AggState;
use crate::vdbe::cast::cast_to;
use crate::vdbe::collation::Collation;
use crate::vdbe::compare::compare;
use crate::vdbe::cursor::CursorSlot;
use crate::vdbe::program::{Instruction, Opcode, Program, P4};
use crate::vdbe::{arithmetic, control, cursor, pragma, result, sorter};
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

    #[error("{opcode}: ephemeral table/index exceeded the maximum row count ({limit})")]
    EphemeralRowLimitExceeded { opcode: &'static str, limit: usize },

    #[error("statement halted with SQLite result code {code}{}", message.as_deref().map(|m| format!(": {m}")).unwrap_or_default())]
    Halted { code: i32, message: Option<String> },

    #[error("failed to flush pending writes on statement commit: {0}")]
    FlushFailed(#[from] crate::pager::PagerError),

    #[error("cannot start a transaction within a transaction")]
    TransactionAlreadyActive,

    #[error("cannot commit - no transaction is active")]
    NoActiveTransactionToCommit,

    #[error("cannot rollback - no transaction is active")]
    NoActiveTransactionToRollback,

    #[error("cannot change journal_mode within a transaction")]
    JournalModeChangeDuringTransaction,
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
///
/// `writer` (#194) is `Some` only for a `Vm` built via
/// [`Vm::with_writable_db`] — the same underlying `Pager` `source`
/// reads through (see `src/pager.rs`'s `impl PageSource for
/// RefCell<Pager>`), kept alongside as a concrete `Rc<RefCell<Pager>>`
/// so `Insert`/`Delete`/`IdxInsert`/`NewRowid` can borrow it mutably for
/// b-tree writes, something a type-erased `Rc<dyn PageSource>` cannot
/// offer back. A `Vm::with_db` (read-only) `VmDb` always has `writer:
/// None`, and every write opcode errors via [`ExecError::NoDatabase`]
/// if it runs against one.
#[derive(Clone)]
pub(crate) struct VmDb {
    pub(crate) source: Rc<dyn PageSource>,
    pub(crate) header: DatabaseHeader,
    pub(crate) writer: Option<Rc<std::cell::RefCell<crate::pager::Pager>>>,
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
#[derive(Debug)]
pub struct Vm {
    registers: Vec<Value>,
    /// Cursor-slot storage: a disjoint address space from `registers`,
    /// so a cursor slot and a register of the same integer index never
    /// alias (Requirement 2). `None` until the slot's `Open*` opcode
    /// runs; each open slot holds one of [`CursorSlot`]'s variants (a
    /// real table cursor, an in-memory ephemeral index, a sorter, or a
    /// single-row pseudo-cursor).
    cursors: Vec<Option<CursorSlot>>,
    /// Aggregate-context storage: a disjoint slot table addressed by
    /// `AggStep`/`AggFinal`'s `P1`, the same shape as `cursors` —
    /// `None` until the first `AggStep` for that slot runs (spec 009
    /// Requirement 12, #241).
    agg_contexts: Vec<Option<AggState>>,
    pub(crate) db: Option<VmDb>,
    rows: Vec<Vec<Value>>,
    pub(crate) once_fired: HashSet<usize>,
    /// Bound parameter values, 0-indexed internally but addressed
    /// 1-based by `Opcode::Variable`'s `P1` (SQLite's
    /// `sqlite3_bind_*` convention) — see [`Vm::param`]/[`Vm::bind_params`].
    params: Vec<Value>,
    /// `false` from a `Transaction` opcode until a matching `AutoCommit`
    /// closes it (#360) — mirrors stock SQLite's per-connection
    /// autocommit flag. Starts `true`: a program with no explicit
    /// `BEGIN` commits at `Halt` exactly as it always has. While `false`,
    /// `Halt` neither commits nor rolls back on its own — the program is
    /// expected to reach an explicit `AutoCommit` first; see `run`'s
    /// `Halt` handling for the "BEGIN with no matching COMMIT/ROLLBACK"
    /// safety fallback.
    pub(crate) autocommit: bool,
}

impl Default for Vm {
    fn default() -> Self {
        Self {
            registers: Vec::new(),
            cursors: Vec::new(),
            agg_contexts: Vec::new(),
            db: None,
            rows: Vec::new(),
            once_fired: HashSet::new(),
            params: Vec::new(),
            autocommit: true,
        }
    }
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
            db: Some(VmDb {
                source,
                header,
                writer: None,
            }),
            ..Self::default()
        }
    }

    /// Builds a `Vm` that can service both `OpenRead` and the write
    /// opcodes (`OpenWrite`/`Insert`/`Delete`/`IdxInsert`/`NewRowid`,
    /// #194) against `pager`. `pager` is wrapped once in a shared
    /// `Rc<RefCell<_>>`: one clone is unsized to `Rc<dyn PageSource>`
    /// for `TableCursor`'s ordinary read traversal (`OpenRead`,
    /// `Rewind`/`Next`/`SeekRowid`/`Column`/`Rowid`, all unchanged from
    /// the read-only path), the other kept concrete so write opcodes can
    /// borrow it mutably — see [`VmDb`]'s doc.
    pub fn with_writable_db(pager: crate::pager::Pager, header: DatabaseHeader) -> Self {
        Self::with_shared_writable_db(Rc::new(std::cell::RefCell::new(pager)), header)
    }

    /// Like [`Vm::with_writable_db`], but for a `pager` a caller already
    /// holds as a shared `Rc<RefCell<_>>` (#360) — needed to run more
    /// than one program against the *same* `Pager` in sequence (e.g.
    /// `BEGIN` / some writes / `COMMIT`), since a fresh `Vm` is built
    /// per program but the transaction they share must see one
    /// `Pager`'s `dirty` set, not each get its own.
    pub fn with_shared_writable_db(
        pager: Rc<std::cell::RefCell<crate::pager::Pager>>,
        header: DatabaseHeader,
    ) -> Self {
        let source: Rc<dyn PageSource> = Rc::clone(&pager) as Rc<dyn PageSource>;
        Self {
            db: Some(VmDb {
                source,
                header,
                writer: Some(pager),
            }),
            ..Self::default()
        }
    }

    /// Binds parameter values for `Opcode::Variable` to read, 1-based
    /// (`values[0]` becomes parameter 1). Replaces any previously bound
    /// values.
    pub fn bind_params(&mut self, values: Vec<Value>) {
        self.params = values;
    }

    /// Reads bound parameter `index` (1-based). `None` for an
    /// out-of-range or never-bound index — `Opcode::Variable`
    /// (`src/vdbe/result.rs::variable`) treats that as NULL.
    pub(crate) fn param(&self, index: i32) -> Option<&Value> {
        let idx = usize::try_from(index).ok()?.checked_sub(1)?;
        self.params.get(idx)
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

    /// Returns the shared write-capable `Pager` handle (#194), erroring
    /// if this `Vm` was built via [`Vm::with_db`] (read-only) rather
    /// than [`Vm::with_writable_db`].
    pub(crate) fn writer(
        &self,
        opcode: &'static str,
    ) -> Result<Rc<std::cell::RefCell<crate::pager::Pager>>, ExecError> {
        let db = self.db.as_ref().ok_or(ExecError::NoDatabase { opcode })?;
        db.writer.clone().ok_or(ExecError::NoDatabase { opcode })
    }

    /// Reads aggregate-context slot `slot`. `None` if no `AggStep` has
    /// run for this slot yet (or the group is empty) — distinct from
    /// `cursor`'s error-on-unopened behavior, since an unaggregated
    /// slot is a legitimate zero-row state, not a malformed program.
    pub(crate) fn agg_context(&self, slot: i32) -> Result<Option<&AggState>, ExecError> {
        let idx = Self::index("agg context read", slot)?;
        Ok(self.agg_contexts.get(idx).and_then(Option::as_ref))
    }

    /// Writes aggregate-context slot `slot`, growing the table with
    /// empty filler as needed — mirrors `set_cursor`'s growth policy,
    /// into the disjoint `agg_contexts` storage.
    pub(crate) fn set_agg_context(&mut self, slot: i32, value: AggState) -> Result<(), ExecError> {
        let idx = Self::index("agg context write", slot)?;
        if idx >= self.agg_contexts.len() {
            self.agg_contexts
                .resize_with(idx.saturating_add(1), || None);
        }
        if let Some(cell) = self.agg_contexts.get_mut(idx) {
            *cell = Some(value);
        }
        Ok(())
    }

    /// Clears aggregate-context slot `slot` back to `None` (#304) —
    /// used by `AggFinal` so a slot's leftover accumulator from one
    /// invocation of a compiled program can't leak into a later
    /// invocation that skips `AggStep` entirely (a zero-row group).
    pub(crate) fn clear_agg_context(&mut self, slot: i32) -> Result<(), ExecError> {
        let idx = Self::index("agg context clear", slot)?;
        if let Some(cell) = self.agg_contexts.get_mut(idx) {
            *cell = None;
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
    let (collation, affinity) = match &instr.p4 {
        P4::CollSeq {
            collation,
            affinity,
        } => (*collation, Affinity::from_p4_byte(*affinity)),
        _ => (Collation::Binary, Affinity::Blob),
    };
    // Affinity applies to *copies* of the operands, not the live
    // registers — a comparison must not mutate the row being compared
    // (spec 008 Requirement 1's comparison-affinity half, #138).
    let mut a = a.clone();
    let mut b = b.clone();
    apply_affinity(&mut a, affinity);
    apply_affinity(&mut b, affinity);
    let ord = compare(&a, &b, collation);
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

/// `Cast` (#142): forces register `P1` to `P2`'s affinity byte via the
/// kernel's `CAST` conversion rule (`src/vdbe/cast.rs`) — never
/// `apply_affinity`'s column-affinity rule, which only converts
/// well-formed numeric text and never touches BLOB or errors instead of
/// truncating. This is `CAST`'s only opcode; `MustBeInt`/`RealAffinity`
/// are guard/coercion opcodes for other purposes and must not be
/// reused here (the bug this ticket fixes).
fn cast(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let affinity_byte = instr.p2 as u8;
    let affinity = Affinity::from_p4_byte(affinity_byte);
    let v = vm.register(instr.p1)?.clone();
    vm.set_register(instr.p1, cast_to(&v, affinity))?;
    Ok(Step::Next)
}

fn dispatch(vm: &mut Vm, pc: usize, instr: &Instruction) -> Result<Step, ExecError> {
    use Opcode::{
        Add, AggFinal, AggStep, AutoCommit, BeginSubrtn, BitAnd, BitNot, BitOr, Blob, Cast, Column,
        Concat, Copy, CreateIndex, CreateTable, CreateView, DecrJumpZero, Delete, Divide,
        DropIndex, DropTable, Eq, Found, Function, Ge, Goto, Gt, Halt, IdxDelete, IdxInsert, IdxLE,
        IdxColumn, IdxLast, IdxNext, IdxPrev, IdxRewind, IdxRowid, IfNot, IfNotZero, IfPos, Init, Insert,
        Int64, Integer, IsNull, Last, Le, Lt, MakeRecord, Multiply, MustBeInt, NewRowid, Next,
        NoConflict, Not, NotNull, Null, NullRow, OffsetLimit, Once, OpenEphemeral, OpenPseudo,
        OpenRead, OpenWrite, Real, RealAffinity, Remainder, ResultRow, Return, Rewind, Rowid,
        SeekIndexEq, SeekRowid, Sequence, SetJournalMode, ShiftLeft, ShiftRight, Sort, SorterData,
        SorterInsert, SorterNext, SorterOpen, SorterSort, String8, Subtract, Transaction, Variable,
    };
    match instr.opcode {
        Init => control::init(instr),
        Goto => control::goto(instr),
        Once => control::once(vm, pc, instr),
        BeginSubrtn => control::begin_subrtn(),
        Return => control::r#return(vm, instr),
        Halt => control::halt(instr),
        Transaction => control::transaction(vm, instr),
        AutoCommit => control::auto_commit(vm, instr),
        SetJournalMode => pragma::set_journal_mode(vm, instr),
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
        Cast => cast(vm, instr),

        Add => arithmetic::add(vm, instr),
        Subtract => arithmetic::subtract(vm, instr),
        Multiply => arithmetic::multiply(vm, instr),
        Divide => arithmetic::divide(vm, instr),
        Remainder => arithmetic::remainder(vm, instr),
        Not => arithmetic::not(vm, instr),
        BitAnd => arithmetic::bit_and(vm, instr),
        BitOr => arithmetic::bit_or(vm, instr),
        ShiftLeft => arithmetic::shift_left(vm, instr),
        ShiftRight => arithmetic::shift_right(vm, instr),
        BitNot => arithmetic::bit_not(vm, instr),
        Concat => arithmetic::concat(vm, instr),

        Integer => result::integer(vm, instr),
        Int64 => result::int64(vm, instr),
        Real => result::real(vm, instr),
        Blob => result::blob(vm, instr),
        Null => result::null(vm, instr),
        String8 => result::string8(vm, instr),
        Variable => result::variable(vm, instr),
        MakeRecord => result::make_record(vm, instr),
        ResultRow => result::result_row(vm, instr),
        Copy => result::copy(vm, instr),

        OpenRead => cursor::open_read(vm, instr),
        OpenWrite => cursor::open_write(vm, instr),
        OpenEphemeral => cursor::open_ephemeral(vm, instr),
        OpenPseudo => cursor::open_pseudo(vm, instr),
        Rewind => cursor::rewind(vm, instr),
        Last => cursor::last(vm, instr),
        Next => cursor::next(vm, instr),
        Column => cursor::column(vm, instr),
        Rowid => cursor::rowid(vm, instr),
        SeekRowid => cursor::seek_rowid(vm, instr),
        SeekIndexEq => cursor::seek_index_eq(vm, instr),
        IdxRowid => cursor::idx_rowid(vm, instr),
        IdxColumn => cursor::idx_column(vm, instr),
        IdxRewind => cursor::idx_rewind(vm, instr),
        IdxLast => cursor::idx_last(vm, instr),
        IdxNext => cursor::idx_next(vm, instr),
        IdxPrev => cursor::idx_prev(vm, instr),
        NullRow => cursor::null_row(vm, instr),
        Sequence => cursor::sequence(vm, instr),
        Found => cursor::found(vm, instr),
        IdxInsert => cursor::idx_insert(vm, instr),
        IdxDelete => cursor::idx_delete(vm, instr),
        IdxLE => cursor::idx_le(vm, instr),
        NoConflict => cursor::no_conflict(vm, instr),
        Delete => cursor::delete(vm, instr),
        Insert => cursor::insert(vm, instr),
        NewRowid => cursor::new_rowid(vm, instr),
        CreateTable => cursor::create_table(vm, instr),
        CreateView => cursor::create_view(vm, instr),
        DropTable => cursor::drop_table(vm, instr),
        CreateIndex => cursor::create_index(vm, instr),
        DropIndex => cursor::drop_index(vm, instr),

        SorterOpen => sorter::sorter_open(vm, instr),
        SorterInsert => sorter::sorter_insert(vm, instr),
        SorterSort | Sort => sorter::sorter_sort(vm, instr),
        SorterNext => sorter::sorter_next(vm, instr),
        SorterData => sorter::sorter_data(vm, instr),

        Function => function(vm, instr),
        AggStep => agg_step(vm, instr),
        AggFinal => agg_final(vm, instr),
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

/// `AggStep` (spec 009, Requirement 12, #241; collation/reset #263):
/// folds the argument registers (a contiguous run starting at `P2`,
/// per `P4`'s `AggFunc { name, arity, collation }` descriptor — same
/// register-window shape as `Function`'s `P4::Str`, plus the
/// collation `min`/`max` compares under) into the aggregate-context
/// slot `P1`, creating a fresh accumulator on the slot's first
/// `AggStep` — or whenever `P5` is nonzero, which discards any prior
/// state for this slot before folding, the same "start a fresh
/// accumulator" behavior as a never-stepped slot. Codegen uses this to
/// begin a new GROUP BY group on a reused slot number without a
/// dedicated reset opcode. No result is produced here — `AggFinal`
/// reads the accumulated state once the group is done.
fn agg_step(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let (name, arity, collation) = match &instr.p4 {
        P4::AggFunc {
            name,
            arity,
            collation,
        } => (name.as_str(), *arity, *collation),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "AggStep",
                reason: format!("expected an AggFunc P4, got {other:?}"),
            })
        }
    };
    let mut args = Vec::with_capacity(arity);
    for i in 0..arity {
        let reg = instr
            .p2
            .checked_add(
                i32::try_from(i).map_err(|_| ExecError::RegisterRangeTooLarge {
                    opcode: "AggStep",
                    count: i32::try_from(arity).unwrap_or(i32::MAX),
                })?,
            )
            .ok_or(ExecError::RegisterOutOfRange {
                opcode: "AggStep",
                index: instr.p2,
            })?;
        args.push(vm.register(reg)?.clone());
    }
    let current = if instr.p5 == 0 {
        vm.agg_context(instr.p1)?.cloned()
    } else {
        None
    };
    let updated = crate::vdbe::aggregate::step(name, current, &args, collation).map_err(|e| {
        ExecError::MalformedInstruction {
            opcode: "AggStep",
            reason: e.to_string(),
        }
    })?;
    vm.set_agg_context(instr.p1, updated)?;
    Ok(Step::Next)
}

/// `AggFinal` (spec 009, Requirement 12, #241): finalizes aggregate-
/// context slot `P1` (via `P4`'s `"name(arity)"` descriptor, arity
/// unused here) and writes the result into register `P3`. A slot never
/// stepped (`P1` holds `None`) finalizes as the aggregate's own
/// zero-row result (`count` → 0, `sum` → NULL) rather than erroring —
/// an empty group is a legitimate outcome, not a malformed program.
///
/// Clears the slot back to `None` after finalizing (#304): a slot
/// number is reused across groups within one query (each new group's
/// first `AggStep` passes `reset: true`, discarding whatever was
/// there), but a slot is also reused across separate *invocations* of
/// the same compiled program — e.g. a correlated aggregate subquery
/// re-run once per outer row (`src/codegen/subquery.rs`'s
/// `compile_scalar_subquery`) — where a zero-row invocation skips
/// `AggStep` entirely (see `compile_grouped_scan`'s
/// `empty_sorter_target`) and goes straight to `AggFinal`. Without
/// clearing here, that zero-row invocation would incorrectly finalize
/// against the *previous* invocation's leftover accumulator instead of
/// its own true zero-row result.
fn agg_final(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let descriptor = match &instr.p4 {
        P4::Str(s) => s.as_str(),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "AggFinal",
                reason: format!("expected a \"name(arity)\" string P4, got {other:?}"),
            })
        }
    };
    let (name, _arity) =
        parse_function_descriptor(descriptor).ok_or_else(|| ExecError::MalformedInstruction {
            opcode: "AggFinal",
            reason: format!("malformed aggregate descriptor {descriptor:?}"),
        })?;
    let state = vm.agg_context(instr.p1)?;
    let result = crate::vdbe::aggregate::finalize(name, state).map_err(|e| {
        ExecError::MalformedInstruction {
            opcode: "AggFinal",
            reason: e.to_string(),
        }
    })?;
    vm.set_register(instr.p3, result)?;
    vm.clear_agg_context(instr.p1)?;
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
/// legitimate instruction count: a full-table scan over a several-hundred-
/// thousand-row table (#112's ~50MB bench fixture) legitimately spends a
/// handful of steps per row, so the cap needs enough headroom for that —
/// previously 1_000_000, which a real ~830k-row scan already exceeded.
const MAX_STEPS: u32 = 50_000_000;

pub fn execute(program: &Program) -> Result<Vec<Vec<Value>>, ExecError> {
    run(Vm::new(), program).map(|(rows, _)| rows)
}

/// Like [`execute`], but binds `params` for `Opcode::Variable` to read
/// (1-based, per [`Vm::bind_params`]) — a program compiled with `?`
/// placeholders (e.g. `WHERE rowid = ?1`, #137) needs this to run
/// correctly; [`execute`] alone leaves every parameter NULL.
pub fn execute_with_params(
    program: &Program,
    params: Vec<Value>,
) -> Result<Vec<Vec<Value>>, ExecError> {
    let mut vm = Vm::new();
    vm.bind_params(params);
    run(vm, program).map(|(rows, _)| rows)
}

/// Like [`execute`], but the `Vm` can service `OpenRead` (cursor
/// opcodes over real tables) against `source`/`header` — see
/// [`Vm::with_db`].
pub fn execute_with_db(
    program: &Program,
    source: Rc<dyn PageSource>,
    header: DatabaseHeader,
) -> Result<Vec<Vec<Value>>, ExecError> {
    run(Vm::with_db(source, header), program).map(|(rows, _)| rows)
}

/// Like [`execute_with_db`], but the `Vm` can also service the write
/// opcodes (#194: `OpenWrite`/`Insert`/`Delete`/`IdxInsert`/`NewRowid`)
/// against `pager` — see [`Vm::with_writable_db`].
pub fn execute_with_writable_db(
    program: &Program,
    pager: crate::pager::Pager,
    header: DatabaseHeader,
) -> Result<Vec<Vec<Value>>, ExecError> {
    run(Vm::with_writable_db(pager, header), program).map(|(rows, _)| rows)
}

/// Combines [`execute_with_db`] and [`execute_with_params`].
pub fn execute_with_db_and_params(
    program: &Program,
    source: Rc<dyn PageSource>,
    header: DatabaseHeader,
    params: Vec<Value>,
) -> Result<Vec<Vec<Value>>, ExecError> {
    let mut vm = Vm::with_db(source, header);
    vm.bind_params(params);
    run(vm, program).map(|(rows, _)| rows)
}

/// Runs one statement's `program` against a `pager` shared across
/// multiple calls (#360) — the piece that makes `BEGIN`/`COMMIT`/
/// `ROLLBACK` mean something: each SQL statement compiles to its own
/// `Program` and gets its own `Vm`, but a transaction spanning several
/// statements needs one `Pager` (for its `dirty` set) and one
/// autocommit flag threaded through all of them. `autocommit_in` is
/// `true` for a connection's first statement (or one issued outside any
/// transaction); pass back the returned flag as the next call's
/// `autocommit_in` to keep the transaction state connected.
pub fn execute_transaction_step(
    program: &Program,
    pager: Rc<std::cell::RefCell<crate::pager::Pager>>,
    header: DatabaseHeader,
    autocommit_in: bool,
) -> Result<(Vec<Vec<Value>>, bool), ExecError> {
    let mut vm = Vm::with_shared_writable_db(pager, header);
    vm.autocommit = autocommit_in;
    run(vm, program)
}

fn run(mut vm: Vm, program: &Program) -> Result<(Vec<Vec<Value>>, bool), ExecError> {
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
            Step::Halt { code: 0, .. } => {
                // A program with no explicit `Transaction` (#194's
                // original behavior, unchanged) treats a successful
                // `Halt` as an implicit commit, flushing any pending
                // write-opcode changes before returning. A `Vm::with_db`
                // (read-only) or a writable `Vm` that never actually
                // wrote anything both take the cheap
                // `writer.is_none()`/`dirty.is_empty()` no-op path.
                //
                // #360: a program that opened an explicit transaction
                // (`Transaction` opcode, `vm.autocommit == false`) and
                // hasn't reached a matching `AutoCommit` yet does
                // neither — one SQL statement is one `Program`/`Vm`
                // (see `execute_transaction_step`), so `BEGIN`'s own
                // `Halt` running with `autocommit == false` is the
                // normal, expected case: the transaction stays open,
                // `vm.autocommit` carries that forward to whichever
                // `Vm` runs the next statement on this same `Pager`.
                if let Some(db) = &vm.db {
                    if vm.autocommit {
                        if let Some(writer) = &db.writer {
                            writer.borrow_mut().flush()?;
                        }
                    }
                }
                return Ok((vm.rows, vm.autocommit));
            }
            Step::Halt { code, message } => return Err(ExecError::Halted { code, message }),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::vdbe::program::Instruction;

    /// `AggStep`'s `P4` for `name(arity)` under BINARY collation —
    /// the common case for hand-assembled tests that aren't
    /// specifically exercising collation.
    fn agg_p4(name: &str, arity: usize) -> P4 {
        P4::AggFunc {
            name: name.to_string(),
            arity,
            collation: Collation::Binary,
        }
    }

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
    fn compare_jump_applies_comparison_affinity_derived_from_both_operands() {
        // Mirrors #138: `WHERE i = '5'` against an INTEGER column
        // compiles a text literal register on one side. Without
        // applying the P4 affinity byte, `compare()` falls back to
        // storage-class ordering (text > numeric) and never jumps;
        // INTEGER affinity coerces the text side to numeric first.
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(5)).unwrap();
        vm.set_register(1, Value::Text("5".to_string().into()))
            .unwrap();
        let eq = Instruction::with_p4(
            Opcode::Eq,
            0,
            99,
            1,
            P4::CollSeq {
                collation: Collation::Binary,
                affinity: Affinity::Integer.to_p4_byte(),
            },
        );
        assert_eq!(
            compare_jump(&vm, &eq, |o| o == Ordering::Equal).unwrap(),
            Step::Jump(99)
        );
        // The source registers must not be mutated by the comparison.
        assert_eq!(
            *vm.register(1).unwrap(),
            Value::Text("5".to_string().into())
        );
    }

    #[test]
    fn real_affinity_coerces_register_on_load_independent_of_comparison() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Text("1.5".to_string().into()))
            .unwrap();
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

    #[test]
    fn agg_step_accumulates_across_repeated_calls_into_the_same_context_slot() {
        let mut vm = Vm::new();
        for v in [Value::Integer(10), Value::Integer(20), Value::Integer(30)] {
            vm.set_register(0, v).unwrap();
            agg_step(
                &mut vm,
                &Instruction::with_p4(Opcode::AggStep, 0, 0, 0, agg_p4("sum", 1)),
            )
            .unwrap();
        }
        agg_final(
            &mut vm,
            &Instruction::with_p4(Opcode::AggFinal, 0, 0, 1, P4::Str("sum(1)".to_string())),
        )
        .unwrap();
        assert_eq!(*vm.register(1).unwrap(), Value::Integer(60));
    }

    #[test]
    fn agg_final_on_a_never_stepped_slot_yields_the_zero_row_result() {
        let mut vm = Vm::new();
        agg_final(
            &mut vm,
            &Instruction::with_p4(Opcode::AggFinal, 0, 0, 1, P4::Str("count(0)".to_string())),
        )
        .unwrap();
        assert_eq!(*vm.register(1).unwrap(), Value::Integer(0));

        agg_final(
            &mut vm,
            &Instruction::with_p4(Opcode::AggFinal, 2, 0, 3, P4::Str("sum(1)".to_string())),
        )
        .unwrap();
        assert_eq!(*vm.register(3).unwrap(), Value::Null);
    }

    #[test]
    fn distinct_agg_context_slots_do_not_alias() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(1)).unwrap();
        agg_step(
            &mut vm,
            &Instruction::with_p4(Opcode::AggStep, 0, 0, 0, agg_p4("count", 1)),
        )
        .unwrap();
        agg_step(
            &mut vm,
            &Instruction::with_p4(Opcode::AggStep, 1, 0, 0, agg_p4("count", 1)),
        )
        .unwrap();
        agg_step(
            &mut vm,
            &Instruction::with_p4(Opcode::AggStep, 1, 0, 0, agg_p4("count", 1)),
        )
        .unwrap();
        agg_final(
            &mut vm,
            &Instruction::with_p4(Opcode::AggFinal, 0, 0, 10, P4::Str("count(0)".to_string())),
        )
        .unwrap();
        agg_final(
            &mut vm,
            &Instruction::with_p4(Opcode::AggFinal, 1, 0, 11, P4::Str("count(0)".to_string())),
        )
        .unwrap();
        assert_eq!(*vm.register(10).unwrap(), Value::Integer(1));
        assert_eq!(*vm.register(11).unwrap(), Value::Integer(2));
    }

    #[test]
    fn agg_step_rejects_a_non_string_p4() {
        let mut vm = Vm::new();
        assert!(matches!(
            agg_step(&mut vm, &Instruction::new(Opcode::AggStep, 0, 0, 0)),
            Err(ExecError::MalformedInstruction {
                opcode: "AggStep",
                ..
            })
        ));
    }

    #[test]
    fn agg_step_rejects_an_unknown_aggregate_name() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(1)).unwrap();
        assert!(matches!(
            agg_step(
                &mut vm,
                &Instruction::with_p4(Opcode::AggStep, 0, 0, 0, agg_p4("median", 1)),
            ),
            Err(ExecError::MalformedInstruction {
                opcode: "AggStep",
                ..
            })
        ));
    }

    /// #263: `P5 != 0` discards any prior state for the slot before
    /// folding — codegen's mechanism for starting a new GROUP BY group
    /// on a reused slot number, without a dedicated reset opcode.
    #[test]
    fn agg_step_with_nonzero_p5_discards_prior_state_before_folding() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(10)).unwrap();
        agg_step(
            &mut vm,
            &Instruction::with_p4(Opcode::AggStep, 0, 0, 0, agg_p4("sum", 1)),
        )
        .unwrap();
        vm.set_register(0, Value::Integer(5)).unwrap();
        let mut reset_instr = Instruction::with_p4(Opcode::AggStep, 0, 0, 0, agg_p4("sum", 1));
        reset_instr.p5 = 1;
        agg_step(&mut vm, &reset_instr).unwrap();
        agg_final(
            &mut vm,
            &Instruction::with_p4(Opcode::AggFinal, 0, 0, 1, P4::Str("sum(1)".to_string())),
        )
        .unwrap();
        // Had the prior `sum(1)` of 10 not been discarded, this would
        // finalize to 15, not 5.
        assert_eq!(*vm.register(1).unwrap(), Value::Integer(5));
    }

    /// #263: `min`/`max` compare under the `AggFunc` P4's collation —
    /// ASCII binary order puts every uppercase letter before every
    /// lowercase one, so a NOCASE `min` over `{'B', 'a'}` must pick
    /// `'a'`, not `'B'`.
    #[test]
    fn agg_step_min_honours_a_nocase_collation() {
        let mut vm = Vm::new();
        for v in [Value::Text("B".into()), Value::Text("a".into())] {
            vm.set_register(0, v).unwrap();
            agg_step(
                &mut vm,
                &Instruction::with_p4(
                    Opcode::AggStep,
                    0,
                    0,
                    0,
                    P4::AggFunc {
                        name: "min".to_string(),
                        arity: 1,
                        collation: Collation::NoCase,
                    },
                ),
            )
            .unwrap();
        }
        agg_final(
            &mut vm,
            &Instruction::with_p4(Opcode::AggFinal, 0, 0, 1, P4::Str("min(1)".to_string())),
        )
        .unwrap();
        assert_eq!(*vm.register(1).unwrap(), Value::Text("a".into()));
    }

    /// #368 tagged MC/DC vector (obligation `exec_258`, decision
    /// `reg < 0 || reg as usize > MAX_REGISTERS`): leaf A (`reg < 0`) true.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__exec_258__v1_negative_register() {
        assert!(matches!(
            Vm::index("Test", -1),
            Err(ExecError::RegisterOutOfRange { index: -1, .. })
        ));
    }

    /// #368 tagged MC/DC vector (obligation `exec_258`): both leaves false.
    /// Independence pair for A against
    /// `mcdc__exec_258__v1_negative_register`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__exec_258__v2_in_range() {
        assert_eq!(Vm::index("Test", 5).unwrap(), 5);
    }

    /// #368 tagged MC/DC vector (obligation `exec_258`): leaf B
    /// (`reg as usize > MAX_REGISTERS`) true, leaf A false. Independence
    /// pair for B against `mcdc__exec_258__v2_in_range`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__exec_258__v3_over_max_registers() {
        let over = (MAX_REGISTERS as i32).saturating_add(1);
        assert!(matches!(
            Vm::index("Test", over),
            Err(ExecError::RegisterOutOfRange { .. })
        ));
    }

    /// #368 tagged MC/DC vector (obligation `exec_406`, decision
    /// `matches!(a, Value::Null) || matches!(b, Value::Null)`): leaf A
    /// true.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__exec_406__v1_left_operand_null() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Null).unwrap();
        vm.set_register(1, Value::Integer(1)).unwrap();
        let eq = Instruction::new(Opcode::Eq, 0, 99, 1);
        assert_eq!(
            compare_jump(&vm, &eq, |o| o == Ordering::Equal).unwrap(),
            Step::Next
        );
    }

    /// #368 tagged MC/DC vector (obligation `exec_406`): both leaves
    /// false. Independence pair for A against
    /// `mcdc__exec_406__v1_left_operand_null`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__exec_406__v2_neither_operand_null() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(1)).unwrap();
        vm.set_register(1, Value::Integer(1)).unwrap();
        let eq = Instruction::new(Opcode::Eq, 0, 99, 1);
        assert_eq!(
            compare_jump(&vm, &eq, |o| o == Ordering::Equal).unwrap(),
            Step::Jump(99)
        );
    }

    /// #368 tagged MC/DC vector (obligation `exec_406`): leaf B true,
    /// leaf A false. Independence pair for B against
    /// `mcdc__exec_406__v2_neither_operand_null`.
    #[test]
    #[allow(non_snake_case)]
    fn mcdc__exec_406__v3_right_operand_null() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(1)).unwrap();
        vm.set_register(1, Value::Null).unwrap();
        let eq = Instruction::new(Opcode::Eq, 0, 99, 1);
        assert_eq!(
            compare_jump(&vm, &eq, |o| o == Ordering::Equal).unwrap(),
            Step::Next
        );
    }
}
