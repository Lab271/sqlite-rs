//! Instruction format and the linear bytecode `Program` (spec 009,
//! Requirement 1). `Opcode` enumerates the full frozen V2 opcode set
//! (`tools/opcodes-v2.json`, 65 opcodes, oracle 3.53.4, #87/#139/#142/#137) —
//! every variant listed here, whether or not `src/vdbe/exec.rs`
//! implements it yet, so the enum stays the single source of truth for
//! "in scope for V2" across #89/#90/#91. `Variable` was added by #137
//! (bound-parameter point lookups) — the harvest now includes a
//! `WHERE id = ?1` query.

use crate::vdbe::Collation;

/// The 65 V2 opcodes, grouped by category to match
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
    OpenWrite,
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
    Insert,
    NewRowid,
    IdxDelete,
    // #207: real-index seek+branch primitive for non-rowid UNIQUE
    // constraint enforcement — like #194's other V3 write-path opcodes
    // above, this postdates the V2 oracle harvest, so it's excluded
    // from `ALL` (never harvested from a V2 `EXPLAIN`) but still fully
    // dispatched and exhaustiveness-checked.
    NoConflict,
    // #243: real secondary-index read path for the planner's join
    // equality-index-selection fast path — like #207's `NoConflict`,
    // postdates the V2 oracle harvest (no query-time index seek existed
    // then), so excluded from `ALL` but fully dispatched and
    // exhaustiveness-checked. `SeekIndexEq` probes an index b-tree for an
    // exact key match (jumping P2 on miss); `IdxRowid` reads the trailing
    // rowid column off the index cursor's currently-seeked entry so
    // codegen can chain into a `SeekRowid` on the table cursor.
    SeekIndexEq,
    IdxRowid,
    // DDL (#215) — schema-mutating statements, each done procedurally in
    // one exec.rs handler rather than decomposed into cursor-driven
    // multi-instruction sequences; never harvested from a V2 oracle
    // EXPLAIN (DDL postdates V2), so excluded from `ALL` like the other
    // V3 write opcodes above.
    CreateTable,
    DropTable,
    CreateIndex,
    DropIndex,
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
    Not,
    BitAnd,
    BitOr,
    ShiftLeft,
    ShiftRight,
    BitNot,
    Concat,
    // #142: CAST forces an affinity via its own dedicated opcode, not
    // by misusing MustBeInt/RealAffinity.
    Cast,
    // function
    Function,
    // aggregate (#241) — postdates the V2 oracle harvest (no GROUP BY
    // codegen existed then), so excluded from `ALL` like the other V3
    // opcodes above, but fully dispatched and exhaustiveness-checked.
    AggStep,
    AggFinal,
    // result
    Integer,
    Int64,
    Real,
    Blob,
    Null,
    String8,
    Variable,
    MakeRecord,
    ResultRow,
    // #208: copies register `P1`'s value into `P2` verbatim — needed
    // when `INSERT ... SELECT` re-projects a `SELECT`-scan's already-
    // populated registers into a target table's schema-column order:
    // `compile_row`'s `MakeRecord` needs one *fresh*, contiguous
    // register per column (mirroring the literal-`Expr` path's
    // `compile_value` calls, which always bump-allocate anew), not the
    // scan's original (non-contiguous, non-reorderable-in-place)
    // registers reused directly. Never harvested from a V2 oracle
    // EXPLAIN (V2 predates any write path), so excluded from `ALL`
    // like the other V3 write opcodes above.
    Copy,
    // sorter
    SorterOpen,
    SorterInsert,
    SorterSort,
    SorterNext,
    SorterData,
    Sort,
}

impl Opcode {
    /// The 65 harvested variants (`tools/opcodes-v2.json`), in
    /// enum-declaration order — `tests/vdbe/opcode_completeness_test.rs`
    /// checks this list against that harvest exactly, so `OpenWrite`/
    /// `Insert`/`NewRowid` (#194, the V3 write path — never harvested
    /// from a V2 oracle EXPLAIN, since V2 predates any write-path
    /// support) are deliberately excluded from `ALL`, not just missing
    /// by omission. `_exhaustive` below is the separate, unrelated
    /// guarantee that every `Opcode` variant (harvested or not) is
    /// handled by at least one match arm somewhere that matches on
    /// `Opcode` exhaustively — it does not require a variant to appear
    /// in `ALL`.
    pub const ALL: [Opcode; 65] = [
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
        Opcode::Not,
        Opcode::BitAnd,
        Opcode::BitOr,
        Opcode::ShiftLeft,
        Opcode::ShiftRight,
        Opcode::BitNot,
        Opcode::Concat,
        Opcode::Cast,
        Opcode::Function,
        Opcode::Integer,
        Opcode::Int64,
        Opcode::Real,
        Opcode::Blob,
        Opcode::Null,
        Opcode::String8,
        Opcode::Variable,
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
        | Opcode::OpenWrite
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
        | Opcode::Insert
        | Opcode::NewRowid
        | Opcode::IdxDelete
        | Opcode::NoConflict
        | Opcode::SeekIndexEq
        | Opcode::IdxRowid
        | Opcode::CreateTable
        | Opcode::DropTable
        | Opcode::CreateIndex
        | Opcode::DropIndex
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
        | Opcode::Not
        | Opcode::BitAnd
        | Opcode::BitOr
        | Opcode::ShiftLeft
        | Opcode::ShiftRight
        | Opcode::BitNot
        | Opcode::Concat
        | Opcode::Cast
        | Opcode::Function
        | Opcode::AggStep
        | Opcode::AggFinal
        | Opcode::Integer
        | Opcode::Int64
        | Opcode::Real
        | Opcode::Blob
        | Opcode::Null
        | Opcode::String8
        | Opcode::Variable
        | Opcode::MakeRecord
        | Opcode::ResultRow
        | Opcode::Copy
        | Opcode::SorterOpen
        | Opcode::SorterInsert
        | Opcode::SorterSort
        | Opcode::SorterNext
        | Opcode::SorterData
        | Opcode::Sort => {}
    }
}

/// One column of a [`SorterOpen`](Opcode::SorterOpen) sort-key
/// descriptor: sort direction plus the collation to compare under —
/// e.g. the harvested `"k(2,-B,B)"` (2 keys, first descending, both
/// BINARY) becomes `[{descending: true, ..}, {descending: false, ..}]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortKeyColumn {
    /// Index into the sorter's record payload this key column reads —
    /// not assumed to be the key's position among `SortKey`'s own
    /// `Vec` (a sort key need not be the record's leading columns).
    pub index: usize,
    pub descending: bool,
    pub collation: Collation,
    /// Whether NULL sorts before non-NULL values for this key, per an
    /// explicit `NULLS FIRST`/`NULLS LAST` clause (or SQLite's default:
    /// NULLs first for ASC, last for DESC, when no clause is given).
    pub nulls_first: bool,
}

/// P4's dynamic type: absent, an integer constant, a string constant
/// (or function/index descriptor), a collation-sequence-plus-affinity
/// descriptor used by the compare opcodes (e.g. `"BINARY-8"`), or a
/// sorter's per-column sort-key descriptor.
#[derive(Debug, Clone, PartialEq)]
pub enum P4 {
    None,
    Int(i64),
    Real(f64),
    Blob(Vec<u8>),
    Str(String),
    CollSeq {
        collation: Collation,
        affinity: u8,
    },
    SortKey(Vec<SortKeyColumn>),
    /// `MakeRecord`'s (#194) per-column affinity string, one
    /// [`crate::vdbe::affinity::Affinity`] byte
    /// (`Affinity::to_p4_byte`'s `'A'..='E'` convention) per source
    /// register, applied before encoding — SQLite's own
    /// `P4_KEYINFO`/affinity-string convention (`sqlite3VdbeMakeLabel`'s
    /// affinity string), modeled minimally as an owned byte string
    /// rather than a dedicated `KeyInfo` struct.
    Affinity(Vec<u8>),
    /// A boolean flag operand — used by `NewRowid` (#194) to request
    /// AUTOINCREMENT handling (checking/bumping `sqlite_sequence`)
    /// when the VDBE layer has no other place to carry that bit.
    Bool(bool),
    /// `CreateTable` (#215): the new table's name and verbatim
    /// `sqlite_master.sql` text (sliced from the original source via the
    /// AST's `span`, not reconstructed from the parsed columns).
    CreateTable {
        name: String,
        sql: String,
    },
    /// `DropTable` (#215): the target table's name/root page, plus every
    /// index on it (`(name, root_page)`) to cascade-drop in the same
    /// statement.
    DropTable {
        name: String,
        root_page: u32,
        indexes: Vec<(String, u32)>,
    },
    /// `CreateIndex` (#215): the new index's name, its target table's
    /// name/root page (to scan and populate entries for pre-existing
    /// rows), verbatim `sqlite_master.sql` text, the indexed columns'
    /// 0-based positions in table-column order, and the `UNIQUE` flag.
    CreateIndex {
        name: String,
        table_name: String,
        table_root_page: u32,
        sql: String,
        column_indices: Vec<usize>,
        unique: bool,
    },
    /// `DropIndex` (#215): the target index's name/root page.
    DropIndex {
        name: String,
        root_page: u32,
    },
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
