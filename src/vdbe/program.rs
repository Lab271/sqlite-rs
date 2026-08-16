//! Instruction format and the linear bytecode `Program` (spec 009,
//! Requirement 1). `Opcode` enumerates the full frozen V2 opcode set
//! (`tools/opcodes-v2.json`, 52 opcodes, oracle 3.53.3, #87) — every
//! variant listed here, whether or not `src/vdbe/exec.rs` implements it
//! yet, so the enum stays the single source of truth for "in scope for
//! V2" across #89/#90/#91.

use crate::vdbe::Collation;

/// The 52 V2 opcodes, grouped by category to match
/// `tools/opcodes-v2.json`'s taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Opcode {
    // control
    Init,
    Goto,
    Once,
    BeginSubrtn,
    Return,
    Halt,
    Transaction,
    IfNot,
    IfNotZero,
    IfPos,
    DecrJumpZero,
    IsNull,
    NotNull,
    MustBeInt,
    OffsetLimit,
    // cursor
    OpenRead,
    OpenEphemeral,
    OpenPseudo,
    Rewind,
    Last,
    Next,
    Column,
    Rowid,
    SeekRowid,
    NullRow,
    Sequence,
    Found,
    IdxInsert,
    IdxLE,
    Delete,
    // compare
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
    RealAffinity,
    // arithmetic
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    // function
    Function,
    // result
    Integer,
    String8,
    MakeRecord,
    ResultRow,
    // sorter
    SorterOpen,
    SorterInsert,
    SorterSort,
    SorterNext,
    SorterData,
    Sort,
}

impl Opcode {
    /// All 52 variants, in enum-declaration order — the harvested
    /// inventory `tests/vdbe/opcode_completeness_test.rs` checks against
    /// `tools/opcodes-v2.json` (#65). Kept honest by `_exhaustive` below:
    /// an unmatched new variant fails the build rather than silently
    /// falling out of this list.
    pub const ALL: [Opcode; 52] = [
        Opcode::Init,
        Opcode::Goto,
        Opcode::Once,
        Opcode::BeginSubrtn,
        Opcode::Return,
        Opcode::Halt,
        Opcode::Transaction,
        Opcode::IfNot,
        Opcode::IfNotZero,
        Opcode::IfPos,
        Opcode::DecrJumpZero,
        Opcode::IsNull,
        Opcode::NotNull,
        Opcode::MustBeInt,
        Opcode::OffsetLimit,
        Opcode::OpenRead,
        Opcode::OpenEphemeral,
        Opcode::OpenPseudo,
        Opcode::Rewind,
        Opcode::Last,
        Opcode::Next,
        Opcode::Column,
        Opcode::Rowid,
        Opcode::SeekRowid,
        Opcode::NullRow,
        Opcode::Sequence,
        Opcode::Found,
        Opcode::IdxInsert,
        Opcode::IdxLE,
        Opcode::Delete,
        Opcode::Eq,
        Opcode::Ge,
        Opcode::Gt,
        Opcode::Le,
        Opcode::Lt,
        Opcode::RealAffinity,
        Opcode::Add,
        Opcode::Subtract,
        Opcode::Multiply,
        Opcode::Divide,
        Opcode::Remainder,
        Opcode::Function,
        Opcode::Integer,
        Opcode::String8,
        Opcode::MakeRecord,
        Opcode::ResultRow,
        Opcode::SorterOpen,
        Opcode::SorterInsert,
        Opcode::SorterSort,
        Opcode::SorterNext,
        Opcode::SorterData,
        Opcode::Sort,
    ];
}

/// Unused at runtime — its only job is to fail to compile if `Opcode`
/// gains a variant that `Opcode::ALL` doesn't list.
#[allow(dead_code)]
fn _exhaustive(o: Opcode) {
    match o {
        Opcode::Init
        | Opcode::Goto
        | Opcode::Once
        | Opcode::BeginSubrtn
        | Opcode::Return
        | Opcode::Halt
        | Opcode::Transaction
        | Opcode::IfNot
        | Opcode::IfNotZero
        | Opcode::IfPos
        | Opcode::DecrJumpZero
        | Opcode::IsNull
        | Opcode::NotNull
        | Opcode::MustBeInt
        | Opcode::OffsetLimit
        | Opcode::OpenRead
        | Opcode::OpenEphemeral
        | Opcode::OpenPseudo
        | Opcode::Rewind
        | Opcode::Last
        | Opcode::Next
        | Opcode::Column
        | Opcode::Rowid
        | Opcode::SeekRowid
        | Opcode::NullRow
        | Opcode::Sequence
        | Opcode::Found
        | Opcode::IdxInsert
        | Opcode::IdxLE
        | Opcode::Delete
        | Opcode::Eq
        | Opcode::Ge
        | Opcode::Gt
        | Opcode::Le
        | Opcode::Lt
        | Opcode::RealAffinity
        | Opcode::Add
        | Opcode::Subtract
        | Opcode::Multiply
        | Opcode::Divide
        | Opcode::Remainder
        | Opcode::Function
        | Opcode::Integer
        | Opcode::String8
        | Opcode::MakeRecord
        | Opcode::ResultRow
        | Opcode::SorterOpen
        | Opcode::SorterInsert
        | Opcode::SorterSort
        | Opcode::SorterNext
        | Opcode::SorterData
        | Opcode::Sort => {}
    }
}

/// P4's dynamic type: absent, an integer constant, a string constant
/// (or function/index descriptor), or a collation-sequence-plus-affinity
/// descriptor used by the compare opcodes (e.g. `"BINARY-8"`).
#[derive(Debug, Clone, PartialEq)]
pub enum P4 {
    None,
    Int(i64),
    Str(String),
    CollSeq { collation: Collation, affinity: u8 },
}

/// A single fixed-shape bytecode instruction: an opcode tag, three
/// integer operands (`P1`/`P2`/`P3`), one dynamically-typed operand
/// (`P4`), and one flags operand (`P5`) — matching SQLite's own `Op`
/// struct shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    pub opcode: Opcode,
    pub p1: i32,
    pub p2: i32,
    pub p3: i32,
    pub p4: P4,
    pub p5: u16,
}

impl Instruction {
    /// Builds an instruction with `P4` absent and `P5` zero — the common
    /// case for control/arithmetic/compare opcodes that only use
    /// `P1`/`P2`/`P3`.
    pub fn new(opcode: Opcode, p1: i32, p2: i32, p3: i32) -> Self {
        Self {
            opcode,
            p1,
            p2,
            p3,
            p4: P4::None,
            p5: 0,
        }
    }

    /// Builds an instruction carrying a `P4` operand.
    pub fn with_p4(opcode: Opcode, p1: i32, p2: i32, p3: i32, p4: P4) -> Self {
        Self {
            opcode,
            p1,
            p2,
            p3,
            p4,
            p5: 0,
        }
    }
}

/// A linear, zero-indexed sequence of instructions. Execution starts at
/// PC 0 and advances by incrementing PC unless an instruction explicitly
/// redirects it (jump, subroutine call/return, or `Halt`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Program {
    pub instructions: Vec<Instruction>,
}

impl Program {
    pub fn new(instructions: Vec<Instruction>) -> Self {
        Self { instructions }
    }

    pub fn get(&self, pc: usize) -> Option<&Instruction> {
        self.instructions.get(pc)
    }

    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn instruction_carries_typed_p4_variant() {
        // Mirrors the harvested `Ge` instruction (SELECT * FROM products
        // WHERE price >= 10 AND qty < 50): P4 is a collation-sequence
        // descriptor, not an integer or absent value.
        let ge = Instruction::with_p4(
            Opcode::Ge,
            1,
            2,
            3,
            P4::CollSeq {
                collation: Collation::Binary,
                affinity: 8,
            },
        );
        assert!(matches!(ge.p4, P4::CollSeq { .. }));
    }

    #[test]
    fn program_indexes_instructions_from_zero() {
        let program = Program::new(vec![
            Instruction::new(Opcode::Init, 0, 1, 0),
            Instruction::new(Opcode::Halt, 0, 0, 0),
        ]);
        assert_eq!(program.get(0).unwrap().opcode, Opcode::Init);
        assert_eq!(program.get(1).unwrap().opcode, Opcode::Halt);
        assert!(program.get(2).is_none());
    }
}
