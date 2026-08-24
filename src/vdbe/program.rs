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
    /// Program entry point: jumps to `P2`, the real first instruction —
    /// mirrors SQLite's own convention of a leading `Init` before the
    /// generated body.
    Init,
    /// Unconditional jump to `P2`.
    Goto,
    /// Runs its body at most once per VDBE invocation, jumping past it
    /// on subsequent passes (used for one-time initialization).
    Once,
    /// Marks the entry point of a subroutine invoked via `Goto`/`Gosub`-
    /// style control flow, paired with `Return`.
    BeginSubrtn,
    /// Returns from a subroutine to the PC saved when it was entered.
    Return,
    /// Stops execution, ending the program (successfully or with the
    /// error carried in its operands).
    Halt,
    /// Begins a read or write transaction on the database named by `P1`.
    Transaction,
    // #360: explicit COMMIT/ROLLBACK — postdates the V2 oracle harvest
    // (V2 had no write path, so no BEGIN/COMMIT/ROLLBACK codegen
    // existed then), so excluded from `ALL` like the other V3 write
    // opcodes below, but fully dispatched and exhaustiveness-checked.
    // `P2` is stock SQLite's convention: 1 = commit, 0 = rollback.
    /// Explicit `COMMIT`/`ROLLBACK` — `P2` selects commit (1) or rollback
    /// (0), stock SQLite's own convention.
    AutoCommit,
    // #388: `PRAGMA journal_mode = WAL|DELETE` -- like `AutoCommit`
    // above, postdates the V2 oracle harvest (no PRAGMA codegen existed
    // then), so excluded from `ALL` but fully dispatched and
    // exhaustiveness-checked. `P1` carries the target mode
    // (`crate::vdbe::pragma::JOURNAL_MODE_WAL`/`JOURNAL_MODE_DELETE`).
    /// `PRAGMA journal_mode = WAL|DELETE`: `P1` carries the target mode
    /// (`crate::vdbe::pragma::JOURNAL_MODE_WAL`/`JOURNAL_MODE_DELETE`).
    SetJournalMode,
    /// Jumps to `P2` if register `P1` is falsy (zero/false, per SQLite's
    /// truthiness rules).
    IfNot,
    /// Jumps to `P2` if register `P1` is not zero.
    IfNotZero,
    /// Jumps to `P2` if register `P1` holds a value greater than zero.
    IfPos,
    /// Decrements register `P1`; jumps to `P2` if the result is zero (or
    /// it was already zero/NULL).
    DecrJumpZero,
    /// Jumps to `P2` if register `P1` is NULL.
    IsNull,
    /// Jumps to `P2` if register `P1` is not NULL.
    NotNull,
    /// Coerces register `P1` to an integer, failing the statement if it
    /// cannot be represented as one.
    MustBeInt,
    /// Computes `LIMIT`/`OFFSET` bookkeeping from registers `P1`
    /// (limit) and `P2` (offset), storing the combined counter in `P3`.
    OffsetLimit,
    // cursor
    /// Opens cursor `P1` for read-only access to the table/index with
    /// root page `P2`.
    OpenRead,
    /// Opens cursor `P1` for read/write access to the table/index with
    /// root page `P2`.
    OpenWrite,
    /// Opens cursor `P1` on a new, empty temporary b-tree (used for
    /// scratch/ephemeral storage, e.g. `DISTINCT`/subquery materialization).
    OpenEphemeral,
    /// Opens cursor `P1` as a pseudo-cursor over a single in-memory
    /// record buffer rather than a real b-tree.
    OpenPseudo,
    /// Positions cursor `P1` at its first entry, jumping to `P2` if the
    /// table/index is empty.
    Rewind,
    /// Positions cursor `P1` at its last entry, jumping to `P2` if the
    /// table/index is empty.
    Last,
    /// Advances cursor `P1` to its next entry, jumping to `P2` if there
    /// was one (i.e. the cursor did not run off the end).
    Next,
    /// Reads column `P2` of cursor `P1`'s current row into register `P3`.
    Column,
    /// Stores cursor `P1`'s current rowid into register `P2`.
    Rowid,
    /// Seeks cursor `P1` to the row with rowid `P3`, jumping to `P2` if
    /// no such row exists.
    SeekRowid,
    /// Sets cursor `P1` to point at a synthetic NULL row (used after a
    /// failed seek so subsequent `Column` reads yield NULL).
    NullRow,
    /// Stores cursor `P1`'s next-available sequence number into register
    /// `P2` (backs `AUTOINCREMENT`-style rowid allocation).
    Sequence,
    /// Seeks cursor `P1` for an entry matching the key built from
    /// registers `P3..P3+P4`, jumping to `P2` if found.
    Found,
    /// Inserts the index entry in register `P2` into cursor `P1`'s
    /// b-tree.
    IdxInsert,
    /// Compares cursor `P1`'s current index key against the key built
    /// from registers `P3..P3+P4`, jumping to `P2` if it is `<=`.
    IdxLE,
    /// Deletes cursor `P1`'s current row.
    Delete,
    /// Inserts the record in register `P2` (keyed by rowid in `P3`) into
    /// cursor `P1`'s table b-tree.
    Insert,
    /// Generates a new, unused rowid for cursor `P1`'s table, storing it
    /// in register `P2`.
    NewRowid,
    /// Deletes cursor `P1`'s current index entry.
    IdxDelete,
    // #207: real-index seek+branch primitive for non-rowid UNIQUE
    // constraint enforcement — like #194's other V3 write-path opcodes
    // above, this postdates the V2 oracle harvest, so it's excluded
    // from `ALL` (never harvested from a V2 `EXPLAIN`) but still fully
    // dispatched and exhaustiveness-checked.
    /// Probes cursor `P1`'s index for a conflicting key built from
    /// registers `P3..P3+P4`, jumping to `P2` if no conflict is found.
    NoConflict,
    // #243: real secondary-index read path for the planner's join
    // equality-index-selection fast path — like #207's `NoConflict`,
    // postdates the V2 oracle harvest (no query-time index seek existed
    // then), so excluded from `ALL` but fully dispatched and
    // exhaustiveness-checked. `SeekIndexEq` probes an index b-tree for an
    // exact key match (jumping P2 on miss); `IdxRowid` reads the trailing
    // rowid column off the index cursor's currently-seeked entry so
    // codegen can chain into a `SeekRowid` on the table cursor.
    /// Probes cursor `P1`'s index b-tree for an exact key match built
    /// from registers `P3..P3+P4`, jumping to `P2` on miss.
    SeekIndexEq,
    /// Reads the trailing rowid column off index cursor `P1`'s
    /// currently-seeked entry into register `P2`.
    IdxRowid,
    // #296: index-ordered scan — walks a matching index's b-tree
    // directly (forward or backward) in place of `Rewind`/`Next` +
    // sorter opcodes, so `ORDER BY <indexed col> [DESC] LIMIT n` never
    // buffers or sorts at all. Like `SeekIndexEq`/`IdxRowid` above,
    // postdates the V2 oracle harvest, so excluded from `ALL` but fully
    // dispatched and exhaustiveness-checked. `IdxRewind`/`IdxLast`
    // mirror `Rewind`/`Last`'s "jump on empty" shape; `IdxNext`/
    // `IdxPrev` mirror `Next`'s "jump on found" shape — see ADR-0020.
    /// Positions index cursor `P1` at its first entry, jumping to `P2`
    /// if the index is empty (mirrors `Rewind`).
    IdxRewind,
    /// Positions index cursor `P1` at its last entry, jumping to `P2`
    /// if the index is empty (mirrors `Last`).
    IdxLast,
    /// Advances index cursor `P1` forward, jumping to `P2` if there was
    /// a next entry (mirrors `Next`).
    IdxNext,
    /// Advances index cursor `P1` backward, jumping to `P2` if there was
    /// a previous entry.
    IdxPrev,
    // DDL (#215) — schema-mutating statements, each done procedurally in
    // one exec.rs handler rather than decomposed into cursor-driven
    // multi-instruction sequences; never harvested from a V2 oracle
    // EXPLAIN (DDL postdates V2), so excluded from `ALL` like the other
    // V3 write opcodes above.
    /// `CREATE TABLE`: registers a new `sqlite_master` row per its `P4`
    /// payload (see [`P4::CreateTable`]).
    CreateTable,
    /// `DROP TABLE`: removes the table and its indexes per its `P4`
    /// payload (see [`P4::DropTable`]).
    DropTable,
    /// `CREATE INDEX`: registers a new `sqlite_master` row and populates
    /// it from pre-existing rows per its `P4` payload (see
    /// [`P4::CreateIndex`]).
    CreateIndex,
    /// `DROP INDEX`: removes the index per its `P4` payload (see
    /// [`P4::DropIndex`]).
    DropIndex,
    /// `CreateView` (#380): registers a `sqlite_master` row with
    /// `type = 'view'` and `rootpage = 0` (a view has no b-tree of its
    /// own) — otherwise identical to `CreateTable`'s single-instruction
    /// shape, carrying its own `P4::CreateView` payload.
    CreateView,
    /// `ANALYZE` (#461, spec 011): populates `sqlite_stat1` for the
    /// table(s) named by its `P4::Analyze` payload, creating that table
    /// first if this is the first `ANALYZE` ever run — same procedural,
    /// single-instruction shape as `CreateTable`/`CreateIndex` rather
    /// than a decomposed cursor-driven scan.
    Analyze,
    // compare
    /// Jumps to `P2` if registers `P1` and `P3` are equal, per `P4`'s
    /// collation/affinity.
    Eq,
    /// Jumps to `P2` if register `P3` is `>=` register `P1`, per `P4`'s
    /// collation/affinity.
    Ge,
    /// Jumps to `P2` if register `P3` is `>` register `P1`, per `P4`'s
    /// collation/affinity.
    Gt,
    /// Jumps to `P2` if register `P3` is `<=` register `P1`, per `P4`'s
    /// collation/affinity.
    Le,
    /// Jumps to `P2` if register `P3` is `<` register `P1`, per `P4`'s
    /// collation/affinity.
    Lt,
    /// Applies REAL affinity to register `P1` in place (converts an
    /// exact integer to floating point).
    RealAffinity,
    // arithmetic
    /// Adds registers `P1` and `P2`, storing the result in `P3`.
    Add,
    /// Subtracts register `P1` from register `P2`, storing the result
    /// in `P3`.
    Subtract,
    /// Multiplies registers `P1` and `P2`, storing the result in `P3`.
    Multiply,
    /// Divides register `P2` by register `P1`, storing the result in
    /// `P3`.
    Divide,
    /// Computes register `P2` modulo register `P1`, storing the result
    /// in `P3`.
    Remainder,
    /// Stores the logical negation of register `P1` into register `P2`.
    Not,
    /// Stores the bitwise AND of registers `P1` and `P2` into `P3`.
    BitAnd,
    /// Stores the bitwise OR of registers `P1` and `P2` into `P3`.
    BitOr,
    /// Left-shifts register `P2` by register `P1`, storing the result
    /// in `P3`.
    ShiftLeft,
    /// Right-shifts register `P2` by register `P1`, storing the result
    /// in `P3`.
    ShiftRight,
    /// Stores the bitwise complement of register `P1` into register
    /// `P2`.
    BitNot,
    /// Concatenates registers `P1` and `P2` (as text), storing the
    /// result in `P3`.
    Concat,
    // #142: CAST forces an affinity via its own dedicated opcode, not
    // by misusing MustBeInt/RealAffinity.
    /// Forces register `P1` to the affinity named by `P4`, in place.
    Cast,
    // function
    /// Calls the scalar function named by `P4` with arguments in
    /// registers `P2..P2+P5`, storing the result in register `P3`.
    Function,
    // aggregate (#241) — postdates the V2 oracle harvest (no GROUP BY
    // codegen existed then), so excluded from `ALL` like the other V3
    // opcodes above, but fully dispatched and exhaustiveness-checked.
    /// Feeds one input row (registers `P2..P2+P5`) into the aggregate
    /// accumulator in register `P3`, per `P4`'s function descriptor.
    AggStep,
    /// Finalizes the aggregate accumulator in register `P1`, replacing
    /// it with the aggregate's final value per `P4`'s function
    /// descriptor.
    AggFinal,
    // result
    /// Stores the integer constant `P1` into register `P2`.
    Integer,
    /// Stores the 64-bit integer constant carried in `P4` into register
    /// `P2`.
    Int64,
    /// Stores the floating-point constant carried in `P4` into register
    /// `P2`.
    Real,
    /// Stores the blob constant carried in `P4` into register `P2`.
    Blob,
    /// Stores NULL into register `P2`.
    Null,
    /// Stores the text constant carried in `P4` into register `P2`.
    String8,
    /// Stores the value bound to parameter `P1` into register `P2`.
    Variable,
    /// Serializes registers `P1..P1+P2` into a record blob (per `P4`'s
    /// affinity string, if any), storing it in register `P3`.
    MakeRecord,
    /// Emits registers `P1..P1+P2` as one output row.
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
    /// Copies register `P1`'s value into register `P2` verbatim.
    Copy,
    // sorter
    /// Opens a sorter on cursor `P1`, keyed per `P4`'s sort-key
    /// descriptor.
    SorterOpen,
    /// Inserts the record in register `P2` into sorter cursor `P1`.
    SorterInsert,
    /// Sorts sorter cursor `P1`'s buffered records and positions it at
    /// the first one, jumping to `P2` if it is empty.
    SorterSort,
    /// Advances sorter cursor `P1` to its next record, jumping to `P2`
    /// if there was one.
    SorterNext,
    /// Stores sorter cursor `P1`'s current record into register `P2`.
    SorterData,
    /// Standalone in-place sort primitive (distinct from the
    /// `SorterOpen`/`SorterInsert`/`SorterSort` cursor-driven pipeline).
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
    pub const ALL: [Opcode; 68] = [
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
        Opcode::AggStep,
        Opcode::AggFinal,
        Opcode::Copy,
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
        | Opcode::AutoCommit
        | Opcode::SetJournalMode
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
        | Opcode::IdxRewind
        | Opcode::IdxLast
        | Opcode::IdxNext
        | Opcode::IdxPrev
        | Opcode::CreateTable
        | Opcode::CreateView
        | Opcode::DropTable
        | Opcode::CreateIndex
        | Opcode::DropIndex
        | Opcode::Analyze
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
    /// Whether this key column sorts descending (`true`) or ascending.
    pub descending: bool,
    /// The collating sequence to compare this key column's values under.
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
    /// No P4 operand.
    None,
    /// An integer constant operand.
    Int(i64),
    /// A floating-point constant operand.
    Real(f64),
    /// A blob constant operand.
    Blob(Vec<u8>),
    /// A string constant, or function/index descriptor, operand.
    Str(String),
    /// A collation-sequence-plus-affinity descriptor used by the
    /// compare opcodes (e.g. `"BINARY-8"`).
    CollSeq {
        /// The collating sequence to compare under.
        collation: Collation,
        /// The comparison affinity byte, per SQLite's affinity codes.
        affinity: u8,
    },
    /// `AggStep`'s `"name(arity)"` descriptor plus the collation
    /// `min`/`max` compares under (#263) — `AggFinal` has no
    /// comparison to perform, so it keeps the plain `Str` descriptor.
    AggFunc {
        /// The aggregate function's name.
        name: String,
        /// The aggregate function's argument count.
        arity: usize,
        /// The collation `min`/`max` compares under.
        collation: Collation,
    },
    /// A sorter's per-column sort-key descriptor.
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
        /// The new table's name.
        name: String,
        /// The verbatim `sqlite_master.sql` text.
        sql: String,
    },
    /// `CreateView` (#380): the new view's name and verbatim
    /// `sqlite_master.sql` text — same shape as `CreateTable`'s payload,
    /// but its own variant (rather than reusing `P4::CreateTable`) so a
    /// `Program`'s P4 operand names the DDL kind it actually came from,
    /// matching `CreateIndex`'s own dedicated variant below.
    CreateView {
        /// The new view's name.
        name: String,
        /// The verbatim `sqlite_master.sql` text.
        sql: String,
    },
    /// `DropTable` (#215): the target table's name/root page, plus every
    /// index on it (`(name, root_page)`) to cascade-drop in the same
    /// statement.
    DropTable {
        /// The target table's name.
        name: String,
        /// The target table's root page.
        root_page: u32,
        /// Every index on the table, as `(name, root_page)`, to
        /// cascade-drop in the same statement.
        indexes: Vec<(String, u32)>,
    },
    /// `CreateIndex` (#215): the new index's name, its target table's
    /// name/root page (to scan and populate entries for pre-existing
    /// rows), verbatim `sqlite_master.sql` text, the indexed columns'
    /// 0-based positions in table-column order, and the `UNIQUE` flag.
    CreateIndex {
        /// The new index's name.
        name: String,
        /// The target table's name.
        table_name: String,
        /// The target table's root page, to scan and populate entries
        /// for pre-existing rows.
        table_root_page: u32,
        /// The verbatim `sqlite_master.sql` text.
        sql: String,
        /// The indexed columns' 0-based positions in table-column order.
        column_indices: Vec<usize>,
        /// Whether the index enforces a `UNIQUE` constraint.
        unique: bool,
    },
    /// `DropIndex` (#215): the target index's name/root page.
    DropIndex {
        /// The target index's name.
        name: String,
        /// The target index's root page.
        root_page: u32,
    },
    /// `Analyze` (#461, spec 011): every table `ANALYZE` should populate
    /// stats for — baked at codegen time from the schema catalog (root
    /// pages, index names/root pages) the same way `CreateIndex`/
    /// `DropTable` bake theirs, so the exec-time handler never needs to
    /// re-resolve names against `sqlite_master`.
    Analyze {
        targets: Vec<AnalyzeTarget>,
    },
}

/// One table `ANALYZE` (#461) populates `sqlite_stat1` for: its name and
/// table-b-tree root page, plus every index on it (name + root page) to
/// walk for index-level stats.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalyzeTarget {
    pub table_name: String,
    pub table_root_page: u32,
    pub indexes: Vec<AnalyzeIndexTarget>,
}

/// One index `ANALYZE` (#461) walks to compute `avg_eq` for.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalyzeIndexTarget {
    pub index_name: String,
    pub root_page: u32,
}

/// A single fixed-shape bytecode instruction: an opcode tag, three
/// integer operands (`P1`/`P2`/`P3`), one dynamically-typed operand
/// (`P4`), and one flags operand (`P5`) — matching SQLite's own `Op`
/// struct shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    /// The instruction's opcode tag.
    pub opcode: Opcode,
    /// First integer operand.
    pub p1: i32,
    /// Second integer operand.
    pub p2: i32,
    /// Third integer operand.
    pub p3: i32,
    /// Dynamically-typed fourth operand.
    pub p4: P4,
    /// Flags operand.
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
    /// The program's instructions, in execution order.
    pub instructions: Vec<Instruction>,
}

impl Program {
    /// Builds a program from its instruction sequence.
    pub fn new(instructions: Vec<Instruction>) -> Self {
        Self { instructions }
    }

    /// Returns the instruction at `pc`, or `None` if `pc` is out of
    /// range.
    pub fn get(&self, pc: usize) -> Option<&Instruction> {
        self.instructions.get(pc)
    }

    /// The number of instructions in the program.
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    /// Whether the program has no instructions.
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
