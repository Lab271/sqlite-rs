//! `Select` AST -> `Program` compilation (spec 009, Requirement 11's
//! surrounding statement shape): `Init -> OpenRead -> Rewind -> [WHERE
//! test, result columns, ResultRow] -> Next -> Halt`, with ORDER BY
//! wired through the sorter opcodes, LIMIT/OFFSET as independent
//! `IfPos`/`DecrJumpZero` counters, and DISTINCT via the in-memory
//! ephemeral index — mirroring `tests/vdbe/cursor_sorter_test.rs`'s
//! hand-assembled shapes.
//!
//! Known simplification: LIMIT/OFFSET compile to two independent
//! counters (`IfPos` to skip the first OFFSET matching rows, then
//! `DecrJumpZero` to stop after LIMIT rows) rather than the single
//! combined budget register `OffsetLimit` computes — `OffsetLimit`
//! itself was already implemented and tested by #89; this ticket just
//! doesn't happen to need it for a correct LIMIT/OFFSET shape.

use thiserror::Error;

use crate::codegen::expr::{
    collation_of, column_index, compile_cond, compile_value, emit_column_read, expr_affinity,
    is_aggregate_call,
};
use crate::codegen::{
    p4_coll_seq, CondTargets, Emitter, Label, RegAlloc, Scope, TableBinding, Target,
};
use crate::parser::ast::{
    BinaryOp, CompoundSelect, Distinctness, Expr, ExprKind, FromClause, FunctionArgs,
    JoinConstraint, JoinOp, Literal, ParamKind, ResultColumn, Select, TableRef, TableRefKind,
};
use crate::parser::tokenizer::Span;
use crate::schema::{rowid_alias_column, IndexSchema, TableSchema};
use crate::vdbe::{
    comparison_affinity, Collation, Instruction, Opcode, Program, SortKeyColumn, P4,
};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CodegenError {
    #[error("SELECT has no FROM clause — not supported by this V2-scope compiler")]
    NoFromClause,

    #[error("unknown column {name:?}")]
    UnknownColumn { name: String },

    /// #237: an unqualified column name in a multi-table `FROM` matched
    /// more than one joined table's schema.
    #[error("ambiguous column name: {name:?}")]
    AmbiguousColumn { name: String },

    #[error("unsupported: {reason}")]
    Unsupported { reason: String },

    /// #195: an `INSERT` row supplied a different number of values than
    /// the target column list names.
    #[error("{table} has {expected} columns but {found} values were supplied")]
    RowShapeMismatch {
        table: String,
        expected: usize,
        found: usize,
    },

    /// #240: a `UNION ALL` arm projected a different number of result
    /// columns than the first arm — SQLite rejects this at compile time
    /// rather than padding/truncating rows.
    #[error(
        "SELECTs to the left and right of UNION ALL do not have the same number of result \
         columns: expected {expected}, found {found}"
    )]
    CompoundColumnMismatch { expected: usize, found: usize },
}

const TABLE_CURSOR: i32 = 0;
const SORT_CURSOR: i32 = 1;
const PSEUDO_CURSOR: i32 = 2;
const DISTINCT_CURSOR: i32 = 3;

/// The scan's cursor numbers, parameterized (rather than the fixed
/// `TABLE_CURSOR`/`SORT_CURSOR`/`PSEUDO_CURSOR`/`DISTINCT_CURSOR`
/// constants) so [`compile_select_scan`] can be embedded inside another
/// statement's program (#208: `INSERT ... SELECT`) without colliding
/// with that statement's own cursor numbers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanCursors {
    pub(crate) table: i32,
    pub(crate) sort: i32,
    pub(crate) pseudo: i32,
    pub(crate) distinct: i32,
}

impl ScanCursors {
    const fn for_standalone_select() -> Self {
        Self {
            table: TABLE_CURSOR,
            sort: SORT_CURSOR,
            pseudo: PSEUDO_CURSOR,
            distinct: DISTINCT_CURSOR,
        }
    }

    /// One full, non-colliding cursor set per `UNION ALL` arm (#240) —
    /// `index` 0 is the compound's first arm, 1.. are `select.compound`
    /// arms in order. Each arm gets 4 cursor numbers to itself so an
    /// arm using its own ORDER BY sort cursor or DISTINCT ephemeral
    /// index never collides with another arm's.
    const fn for_arm(index: usize) -> Self {
        let base = (index as i32).saturating_mul(4);
        Self {
            table: base,
            sort: base.saturating_add(1),
            pseudo: base.saturating_add(2),
            distinct: base.saturating_add(3),
        }
    }
}

mod aggregate;
mod entry;
mod eqp;
mod index_scan;
mod join_access;
mod join_full;
mod joins;
mod limit_scan;
mod order_by;
mod projection;

pub use entry::{compile_select, compile_select_compound, compile_select_with_catalog};
pub use eqp::{explain_query_plan, EqpRow};
pub use joins::compile_select_joined;

pub(crate) use entry::{
    compile_select_scan, select_result_column_count, select_result_column_count_joined,
};
pub(crate) use joins::compile_select_joined_scan;
