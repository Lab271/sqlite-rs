//! Cursor opcodes (spec 009, Requirement 4): real table cursors over
//! V1's `TableCursor` (`OpenRead`/`Rewind`/`Last`/`Next`/`Column`/
//! `Rowid`/`SeekRowid`/`NullRow`), an in-memory ephemeral index for
//! DISTINCT (`OpenEphemeral`/`Sequence`/`Found`/`IdxInsert`/`IdxLE`/
//! `Delete`, per the epic's #87 scope decision — never the on-disk file
//! format), an in-memory ephemeral **table** (#257, `OpenEphemeral` with
//! `P5` nonzero) that materializes a subquery-in-FROM and is then scanned
//! with the same `Rewind`/`Last`/`Next`/`Column`/`Rowid`/`Insert` opcodes
//! a real table cursor uses, and a single-row pseudo-cursor (`OpenPseudo`)
//! that lets `Column` read an already-computed record (the sorter's
//! output row) without a special case. A real secondary-index read
//! cursor (`OpenRead` with `P5` nonzero) supports both a one-shot point
//! lookup (`SeekIndexEq`/`IdxRowid`, #243) and a full sequential walk
//! (`IdxRewind`/`IdxLast`/`IdxNext`/`IdxPrev`, #296) for an
//! index-ordered `ORDER BY` scan — see [`IndexReadState`]'s doc.
//!
//! Register/cursor-slot conventions used by this module's opcodes (this
//! ticket's own choice — codegen, #91, is what will actually decide
//! operand layout against the pinned oracle's `EXPLAIN` output; nothing
//! here claims byte-for-byte parity with a harvested instruction's
//! P1..P5, only with the opcode's *semantics*):
//! - `OpenRead(p1=cursor, p2=root page)`
//! - `OpenEphemeral(p1=cursor)` — key-column count isn't needed by this
//!   in-memory implementation (the whole register range passed to
//!   `Found`/`IdxInsert` *is* the key), so `P2` is unused here. `P5`
//!   nonzero (#257) opens the table-mode variant instead (see below).
//! - `OpenPseudo(p1=cursor, p2=register holding the row's record blob)`
//! - `Rewind`/`Last(p1=cursor, p2=jump target if the table is empty)`
//! - `Next(p1=cursor, p2=jump target if another row was found)` —
//!   mirrors the oracle's own `OP_Next` shape: jump back into the loop
//!   body on success, fall through to end the loop on exhaustion.
//! - `Column(p1=cursor, p2=column index, p3=dest register)`
//! - `Rowid(p1=cursor, p2=dest register)`
//! - `SeekRowid(p1=cursor, p2=jump target if not found, p3=register
//!   holding the target rowid)`
//! - `NullRow(p1=cursor)`
//! - `Sequence(p1=cursor, p2=dest register)` — also works on a table-mode
//!   ephemeral cursor (#257), handing out fresh rowids starting at `1`.
//! - `Found(p1=cursor, p2=jump target if the key is present, p3=first
//!   key register, p4=Int(key column count))`
//! - `IdxInsert(p1=cursor, p2=first key register, p4=Int(key column
//!   count))`
//! - `IdxLE(p1=cursor, p2=jump target, p3=first key register,
//!   p4=Int(key column count))` — see [`idx_le`]'s doc for this
//!   opcode's known scope limitation.
//! - `Delete(p1=cursor)` — deletes the entry `Found`/`IdxInsert` most
//!   recently probed/inserted on this cursor.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::btree::{self, IndexCursor, TableCursor, TableRow};
use crate::record::{
    decode_column, decode_record, decode_serial_value, encode_record, parse_header_into,
    TextEncoding, Value,
};
use crate::vdbe::exec::{to_pc, ExecError, Step, Vm};
use crate::vdbe::program::{Instruction, P4};
use crate::vdbe::{compare, Collation};

/// One open cursor slot: a real table cursor, an in-memory ephemeral
/// index, a sorter (state owned by `src/vdbe/sorter.rs`, re-exported
/// here so `Vm`'s single cursor-slot table can hold all cursor kinds),
/// or a single-row pseudo-cursor.
#[derive(Debug)]
pub(crate) enum CursorSlot {
    Table(TableCursorState),
    /// A real index b-tree write cursor (#194) opened by `OpenWrite`
    /// with `P5` nonzero — `root_page` is the index (or WITHOUT ROWID
    /// table) b-tree's root page. Unlike `Table`, this slot carries no
    /// traversal position: `IdxInsert`'s real-cursor path is a
    /// stateless one-shot `insert_entry` call, so there is nothing to
    /// track between opcodes.
    IndexWrite {
        root_page: u32,
    },
    /// A real index b-tree read cursor (#243) opened by `OpenRead` with
    /// `P5` nonzero — the query-time counterpart to `IndexWrite`. Unlike
    /// `IndexWrite`, this slot carries real traversal state (#296
    /// extended it beyond #243's original one-shot `SeekIndexEq` probe
    /// to a full persisted [`IndexCursor`]) — see [`IndexReadState`]'s
    /// own doc.
    IndexRead(IndexReadState),
    Ephemeral(EphemeralState),
    /// An in-memory ephemeral **table** cursor (#257) — opened by
    /// `OpenEphemeral` with `P5` nonzero, unlike the index-mode
    /// [`CursorSlot::Ephemeral`] above (`P5` zero/default). Backs a
    /// materialized subquery-in-FROM: rows are appended via `Insert`
    /// (decoding the `MakeRecord`-encoded payload, same as a real table
    /// cursor) and then scanned with `Rewind`/`Next`/`Column`/`Rowid`
    /// exactly like [`CursorSlot::Table`], just without any on-disk
    /// b-tree backing it.
    EphemeralTable(EphemeralTableState),
    Pseudo {
        register: i32,
    },
    Sorter(crate::vdbe::sorter::SorterState),
}

impl CursorSlot {
    pub(crate) fn type_name(&self) -> &'static str {
        match self {
            CursorSlot::Table(_) => "table cursor",
            CursorSlot::IndexWrite { .. } => "index write cursor",
            CursorSlot::IndexRead(_) => "index read cursor",
            CursorSlot::Ephemeral(_) => "ephemeral cursor",
            CursorSlot::EphemeralTable(_) => "ephemeral table cursor",
            CursorSlot::Pseudo { .. } => "pseudo cursor",
            CursorSlot::Sorter(_) => "sorter cursor",
        }
    }
}

/// A real cursor over `src/btree`'s table b-tree, plus the traversal
/// state `Column`/`Rowid` read from: the row `Rewind`/`Next`/`Last`/
/// `SeekRowid` most recently positioned on (`None` once exhausted), and
/// whether `NullRow` has forced this slot to read as an all-NULL row
/// (used by e.g. an outer-join-style probe that found no match — `Next`/
/// `Rewind` clear it again on the next real positioning call).
pub(crate) struct TableCursorState {
    cursor: TableCursor<Rc<dyn crate::vfs::PageSource>>,
    current: Option<TableRow>,
    forced_null: bool,
    /// The table b-tree's root page (#194) — recorded so `Insert`/
    /// `Delete`/`NewRowid` know which b-tree to write to without a
    /// separate cursor-slot variant. Populated by both `OpenRead` and
    /// `OpenWrite`; a read-only cursor never uses it (no write opcode
    /// runs against it), so it costs nothing on the read-only path.
    root_page: u32,
    /// `current`'s parsed header (#458), computed lazily by the first
    /// `Column` read of a row and reused by every later `Column` read of
    /// the *same* row — invalidated (never left stale) by
    /// [`Self::set_current`], the only way `current` is ever reassigned.
    /// Never itself replaced with a fresh `RowHeaderCache`, so its
    /// backing `Vec` allocation is reused across every row this cursor
    /// visits rather than allocated and freed per row (see its own doc —
    /// that per-row alloc/free was a measured *regression* on the
    /// `full_scan` bench versus no caching at all before this was
    /// switched from `Option<RowHeaderCache>` to this always-present,
    /// reuse-the-allocation shape).
    header_cache: RowHeaderCache,
}

impl TableCursorState {
    /// The sole setter for `current` — always paired with invalidating
    /// `header_cache`, so a cache can never survive its row.
    fn set_current(&mut self, row: Option<TableRow>) {
        self.current = row;
        self.header_cache.invalidate();
    }
}

impl std::fmt::Debug for TableCursorState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TableCursorState")
            .field("current", &self.current)
            .field("forced_null", &self.forced_null)
            .finish_non_exhaustive()
    }
}

/// A record payload's header, parsed once (#458): each column's serial
/// type paired with the byte offset of its body within the row's
/// payload. Lets repeated `Column` opcodes against the same row look up
/// an offset directly instead of re-walking the header from byte 0 every
/// time.
///
/// `valid` (rather than an `Option<RowHeaderCache>` on the owning cursor
/// state) is deliberate: a fresh `Vec` per row — the first, simpler
/// implementation of this cache — measured *slower* than no cache at all
/// on the `full_scan` bench (#458), because it traded a cheap per-column
/// header re-walk for a per-row heap allocation. Keeping one `RowHeaderCache`
/// (and its `Vec`'s capacity) alive for the cursor's whole lifetime and
/// just marking it stale on `set_current` avoids that churn — `ensure`
/// reuses the existing allocation via `Vec::clear` instead of
/// reallocating.
#[derive(Debug, Default)]
struct RowHeaderCache {
    entries: Vec<(u64, usize)>,
    valid: bool,
}

impl RowHeaderCache {
    fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Parses `payload`'s header into `self.entries` if not already
    /// valid for the current row; a no-op otherwise.
    fn ensure(&mut self, payload: &[u8]) -> Result<(), crate::record::RecordError> {
        if !self.valid {
            parse_header_into(payload, &mut self.entries)?;
            self.valid = true;
        }
        Ok(())
    }

    fn column(
        &self,
        payload: &[u8],
        idx: usize,
        encoding: TextEncoding,
    ) -> Result<Value, crate::record::RecordError> {
        debug_assert!(self.valid, "column() called before ensure()");
        match self.entries.get(idx) {
            Some(&(serial_type, offset)) => {
                decode_serial_value(serial_type, payload, offset, encoding).map(|(v, _)| v)
            }
            None => Ok(Value::Null),
        }
    }
}

/// A real secondary-index b-tree read cursor (#243), extended by #296 to
/// also carry a persistent [`IndexCursor`] traversal position — `current`
/// is the row `SeekIndexEq`/`IdxRewind`/`IdxLast`/`IdxNext`/`IdxPrev` most
/// recently positioned on (`None` before any positioning call, on a
/// `SeekIndexEq` miss, or once a scan is exhausted), the same shape
/// `TableCursorState::current` uses for a table cursor. `IdxRowid` reads
/// the trailing rowid column out of `current`'s decoded key — for an
/// ordinary secondary index that column is always the referenced table's
/// rowid (see [`IndexRow`]'s doc); this cursor is never used against a
/// `WITHOUT ROWID` table's own storage, where that wouldn't hold.
pub(crate) struct IndexReadState {
    root_page: u32,
    cursor: IndexCursor<Rc<dyn crate::vfs::PageSource>>,
    current: Option<crate::btree::IndexRow>,
    /// Same role as [`TableCursorState::header_cache`] (#458): `current`'s
    /// parsed header, invalidated whenever `current` is reassigned.
    header_cache: RowHeaderCache,
}

impl IndexReadState {
    /// The sole setter for `current` — see
    /// [`TableCursorState::set_current`]'s doc.
    fn set_current(&mut self, row: Option<crate::btree::IndexRow>) {
        self.current = row;
        self.header_cache.invalidate();
    }
}

impl std::fmt::Debug for IndexReadState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexReadState")
            .field("root_page", &self.root_page)
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

/// The in-memory `BTreeMap` backing DISTINCT's ephemeral index (#87):
/// entries keyed by the encoded record bytes of the probed/inserted
/// column range (spec 003's format, reused byte-for-byte — same encoder
/// `MakeRecord` uses), never touching the on-disk page format.
/// `sequence` is a monotonic counter `Sequence` hands out (independent
/// of the dedup key), and `last_key` records the key `Found`/`IdxInsert`
/// most recently touched, so a following `Delete` (per spec 009 Req 4's
/// "insert then delete the just-produced duplicate" DISTINCT dance)
/// knows which entry to remove without a separate register operand.
/// Row-count ceiling on an in-memory ephemeral table/index (#269): both
/// back a plain `Vec`/`BTreeMap` with no spill-to-disk, unlike real
/// SQLite's temp b-trees, so an unbounded subquery-in-FROM (#257) or a
/// correlated `IN (SELECT ...)` rebuilding its index per outer row
/// (`compile_in_subquery`, `src/codegen/subquery.rs`) could otherwise
/// grow memory without limit. Sized in the same order of magnitude as
/// `crate::btree::MAX_PAGES_VISITED`; not currently configurable, matching
/// this codebase's other hardcoded limits (`MAX_REGISTERS`, `MAX_STEPS`).
#[cfg(not(test))]
pub(crate) const MAX_EPHEMERAL_ROWS: usize = 1_000_000;
/// Kept small under test so the limit-exceeded regression tests don't have
/// to insert a million rows to exercise the check.
#[cfg(test)]
pub(crate) const MAX_EPHEMERAL_ROWS: usize = 8;

#[derive(Debug, Default)]
pub(crate) struct EphemeralState {
    entries: BTreeMap<Vec<u8>, Vec<Value>>,
    sequence: i64,
    last_key: Option<Vec<u8>>,
}

/// Backing store for [`CursorSlot::EphemeralTable`] (#257): rows appended
/// in insertion order, each tagged with the rowid `Insert`'s caller
/// computed (codegen assigns sequential rowids starting at 1 — see
/// `src/codegen/subquery.rs`'s FROM-subquery materialization). `pos` is
/// the row index `Rewind`/`Last`/`Next` most recently positioned on
/// (`None` before any positioning call or once exhausted), mirroring
/// `TableCursorState::current`.
#[derive(Debug)]
pub(crate) struct EphemeralTableState {
    rows: Vec<(i64, Vec<Value>)>,
    pos: Option<usize>,
    /// Monotonic counter `Sequence` hands out (#257) — codegen uses it to
    /// assign each materialized row a fresh rowid before `Insert`,
    /// mirroring how the index-mode [`EphemeralState::sequence`] is used
    /// for DISTINCT. Starts at `1` (rather than `0`, unlike
    /// `EphemeralState::sequence`) to match a real table's first rowid.
    sequence: i64,
}

impl Default for EphemeralTableState {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            pos: None,
            sequence: 1,
        }
    }
}

// Methods rather than free functions so the borrow of `self` elides — see the
// note on the equivalent helpers in sorter.rs.
impl Vm {
    fn table_cursor_mut(
        &mut self,
        slot: i32,
        opcode: &'static str,
    ) -> Result<&mut TableCursorState, ExecError> {
        match self.cursor_mut(slot)? {
            CursorSlot::Table(state) => Ok(state),
            other => Err(ExecError::CursorTypeMismatch {
                opcode,
                slot,
                found: other.type_name(),
                expected: "table cursor",
            }),
        }
    }

    fn ephemeral_mut(
        &mut self,
        slot: i32,
        opcode: &'static str,
    ) -> Result<&mut EphemeralState, ExecError> {
        match self.cursor_mut(slot)? {
            CursorSlot::Ephemeral(state) => Ok(state),
            other => Err(ExecError::CursorTypeMismatch {
                opcode,
                slot,
                found: other.type_name(),
                expected: "ephemeral cursor",
            }),
        }
    }

    fn ephemeral_table_mut(
        &mut self,
        slot: i32,
        opcode: &'static str,
    ) -> Result<&mut EphemeralTableState, ExecError> {
        match self.cursor_mut(slot)? {
            CursorSlot::EphemeralTable(state) => Ok(state),
            other => Err(ExecError::CursorTypeMismatch {
                opcode,
                slot,
                found: other.type_name(),
                expected: "ephemeral table cursor",
            }),
        }
    }
}

fn p4_count(instr: &Instruction, opcode: &'static str) -> Result<usize, ExecError> {
    match &instr.p4 {
        P4::Int(n) => usize::try_from(*n).map_err(|_| ExecError::MalformedInstruction {
            opcode,
            reason: format!("negative key column count {n}"),
        }),
        other => Err(ExecError::MalformedInstruction {
            opcode,
            reason: format!("expected an integer P4 (key column count), got {other:?}"),
        }),
    }
}

fn read_register_range(
    vm: &Vm,
    start: i32,
    count: usize,
    opcode: &'static str,
) -> Result<Vec<Value>, ExecError> {
    let mut values = Vec::with_capacity(count);
    for i in 0..count {
        let reg = start
            .checked_add(
                i32::try_from(i).map_err(|_| ExecError::RegisterRangeTooLarge {
                    opcode,
                    count: count as i32,
                })?,
            )
            .ok_or(ExecError::RegisterOutOfRange {
                opcode,
                index: start,
            })?;
        values.push(vm.register(reg)?.clone());
    }
    Ok(values)
}

/// `OpenRead`: opens a real read cursor on `P2` (the table's root page)
/// into cursor slot `P1`, sharing the `Vm`'s attached database page
/// source (see `Vm::with_db`) with every other open `OpenRead` cursor.
/// `P5` nonzero (#243, mirroring `OpenWrite`'s own `P5` dispatch) opens a
/// [`CursorSlot::IndexRead`] instead — a real secondary-index b-tree read
/// cursor for `SeekIndexEq`, rather than a table cursor.
pub fn open_read(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let root_page = u32::try_from(instr.p2).map_err(|_| ExecError::MalformedInstruction {
        opcode: "OpenRead",
        reason: format!("invalid root page {}", instr.p2),
    })?;
    let db = vm.db()?;
    if instr.p5 != 0 {
        let usable_size = db.header.usable_page_size();
        let cursor = IndexCursor::new(Rc::clone(&db.source), usable_size, root_page);
        vm.set_cursor(
            instr.p1,
            CursorSlot::IndexRead(IndexReadState {
                root_page,
                cursor,
                current: None,
                header_cache: RowHeaderCache::default(),
            }),
        )?;
        return Ok(Step::Next);
    }
    let cursor = TableCursor::new(Rc::clone(&db.source), &db.header, root_page);
    vm.set_cursor(
        instr.p1,
        CursorSlot::Table(TableCursorState {
            cursor,
            current: None,
            forced_null: false,
            root_page,
            header_cache: RowHeaderCache::default(),
        }),
    )?;
    Ok(Step::Next)
}

/// `OpenWrite` (#194): opens a write-capable cursor into slot `P1` on
/// root page `P2`. `P5` selects the b-tree kind: `0` (default) opens a
/// table cursor — the same `CursorSlot::Table` `OpenRead` uses (so
/// `Rewind`/`Next`/`SeekRowid`/`Column`/`Rowid` all work unchanged on a
/// write cursor too, matching decision 6's "`Delete` reads the
/// cursor's current position" requirement); nonzero opens a
/// [`CursorSlot::IndexWrite`] for `IdxInsert`'s real (non-ephemeral)
/// path. Requires a `Vm` built via [`Vm::with_writable_db`] — errors
/// with [`ExecError::NoDatabase`] against a read-only `Vm::with_db`.
pub fn open_write(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    // Fails fast if this `Vm` has no writer, before opening the slot.
    vm.writer("OpenWrite")?;
    let root_page = u32::try_from(instr.p2).map_err(|_| ExecError::MalformedInstruction {
        opcode: "OpenWrite",
        reason: format!("invalid root page {}", instr.p2),
    })?;
    if instr.p5 != 0 {
        vm.set_cursor(instr.p1, CursorSlot::IndexWrite { root_page })?;
        return Ok(Step::Next);
    }
    let db = vm.db()?;
    let cursor = TableCursor::new(Rc::clone(&db.source), &db.header, root_page);
    vm.set_cursor(
        instr.p1,
        CursorSlot::Table(TableCursorState {
            cursor,
            current: None,
            forced_null: false,
            root_page,
            header_cache: RowHeaderCache::default(),
        }),
    )?;
    Ok(Step::Next)
}

/// `OpenEphemeral`: opens an empty in-memory ephemeral index (DISTINCT's
/// dedup table, #87) into cursor slot `P1`. `P5` nonzero (#257, mirroring
/// `OpenRead`/`OpenWrite`'s own table-vs-index `P5` dispatch) instead
/// opens a [`CursorSlot::EphemeralTable`] — an ephemeral table cursor for
/// a materialized subquery-in-FROM.
pub fn open_ephemeral(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    if instr.p5 != 0 {
        vm.set_cursor(
            instr.p1,
            CursorSlot::EphemeralTable(EphemeralTableState::default()),
        )?;
        return Ok(Step::Next);
    }
    vm.set_cursor(instr.p1, CursorSlot::Ephemeral(EphemeralState::default()))?;
    Ok(Step::Next)
}

/// `OpenPseudo`: opens a single-row pseudo-cursor into slot `P1` that
/// re-presents register `P2`'s record blob as a cursor row, so `Column`
/// needs no special case for sorter-sourced (or otherwise
/// already-computed) rows.
pub fn open_pseudo(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    vm.set_cursor(instr.p1, CursorSlot::Pseudo { register: instr.p2 })?;
    Ok(Step::Next)
}

/// `Rewind`: positions cursor `P1` at its first row, jumping to `P2` if
/// the table is empty (mirrors the oracle's own `OP_Rewind` shape). Works
/// against both a real [`CursorSlot::Table`] and (#257) an in-memory
/// [`CursorSlot::EphemeralTable`].
pub fn rewind(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let found = match vm.cursor_mut(instr.p1)? {
        CursorSlot::Table(state) => {
            state.forced_null = false;
            let row = state
                .cursor
                .first()
                .map_err(|e| ExecError::MalformedInstruction {
                    opcode: "Rewind",
                    reason: e.to_string(),
                })?;
            state.set_current(row);
            state.current.is_some()
        }
        CursorSlot::EphemeralTable(state) => {
            state.pos = if state.rows.is_empty() { None } else { Some(0) };
            state.pos.is_some()
        }
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode: "Rewind",
                slot: instr.p1,
                found: other.type_name(),
                expected: "table or ephemeral table cursor",
            })
        }
    };
    Ok(if found {
        Step::Next
    } else {
        Step::Jump(to_pc(instr.p2))
    })
}

/// `Last`: positions cursor `P1` at its last row (highest rowid),
/// jumping to `P2` if the table is empty. Works against both a real
/// [`CursorSlot::Table`] and (#257) an in-memory
/// [`CursorSlot::EphemeralTable`].
pub fn last(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let found = match vm.cursor_mut(instr.p1)? {
        CursorSlot::Table(state) => {
            state.forced_null = false;
            let row = state
                .cursor
                .last()
                .map_err(|e| ExecError::MalformedInstruction {
                    opcode: "Last",
                    reason: e.to_string(),
                })?;
            state.set_current(row);
            state.current.is_some()
        }
        CursorSlot::EphemeralTable(state) => {
            state.pos = state.rows.len().checked_sub(1);
            state.pos.is_some()
        }
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode: "Last",
                slot: instr.p1,
                found: other.type_name(),
                expected: "table or ephemeral table cursor",
            })
        }
    };
    Ok(if found {
        Step::Next
    } else {
        Step::Jump(to_pc(instr.p2))
    })
}

/// `Next`: advances cursor `P1` to the following row, jumping to `P2`
/// (typically back to the loop body's start) if another row was found —
/// falls through (ending the loop) once exhausted. Works against both a
/// real [`CursorSlot::Table`] and (#257) an in-memory
/// [`CursorSlot::EphemeralTable`].
pub fn next(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let found = match vm.cursor_mut(instr.p1)? {
        CursorSlot::Table(state) => {
            let row = state
                .cursor
                .next()
                .map_err(|e| ExecError::MalformedInstruction {
                    opcode: "Next",
                    reason: e.to_string(),
                })?;
            state.set_current(row);
            state.current.is_some()
        }
        CursorSlot::EphemeralTable(state) => {
            let next_pos = state.pos.map(|p| p.saturating_add(1)).unwrap_or(0);
            state.pos = if next_pos < state.rows.len() {
                Some(next_pos)
            } else {
                None
            };
            state.pos.is_some()
        }
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode: "Next",
                slot: instr.p1,
                found: other.type_name(),
                expected: "table or ephemeral table cursor",
            })
        }
    };
    Ok(if found {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// Reads column `idx` of cursor `slot`'s current row. For a table/pseudo
/// cursor this decodes only column `idx` out of the row's record payload
/// (#439) rather than the whole record, so a `WHERE`, `SET`, or
/// `SELECT`-list read pays only for the columns it actually names — a
/// row a `WHERE` filter rejects before reading later columns never has
/// those columns decoded at all.
fn read_row_column(
    vm: &mut Vm,
    slot: i32,
    idx: usize,
    opcode: &'static str,
) -> Result<Value, ExecError> {
    // Pseudo and ephemeral-table cursors need no header cache (a pseudo
    // cursor's record blob is already fully decoded per read; an
    // ephemeral-table row is stored as already-decoded `Value`s) — handle
    // them here against an immutable borrow, so the cache-bearing arms
    // below can take a mutable one without the two ever overlapping.
    match vm.cursor(slot)? {
        CursorSlot::Pseudo { register } => {
            let register = *register;
            return match vm.register(register)? {
                Value::Blob(bytes) => decode_column(bytes, idx, TextEncoding::Utf8).map_err(|e| {
                    ExecError::MalformedInstruction {
                        opcode,
                        reason: e.to_string(),
                    }
                }),
                other => Err(ExecError::MalformedInstruction {
                    opcode,
                    reason: format!("pseudo-cursor register holds {other:?}, not a record blob"),
                }),
            };
        }
        CursorSlot::EphemeralTable(state) => {
            return Ok(state
                .pos
                .and_then(|p| state.rows.get(p))
                .ok_or(ExecError::MalformedInstruction {
                    opcode,
                    reason: "cursor has no current row".to_string(),
                })?
                .1
                .get(idx)
                .cloned()
                .unwrap_or(Value::Null));
        }
        CursorSlot::Table(_) | CursorSlot::IndexRead(_) => {}
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode,
                slot,
                found: other.type_name(),
                expected: "table, pseudo, ephemeral table, or index read cursor",
            })
        }
    }

    match vm.cursor_mut(slot)? {
        CursorSlot::Table(state) => {
            if state.forced_null {
                return Ok(Value::Null);
            }
            let payload = &state
                .current
                .as_ref()
                .ok_or(ExecError::MalformedInstruction {
                    opcode,
                    reason: "cursor has no current row".to_string(),
                })?
                .payload;
            state
                .header_cache
                .ensure(payload)
                .map_err(|e| ExecError::MalformedInstruction {
                    opcode,
                    reason: e.to_string(),
                })?;
            state
                .header_cache
                .column(payload, idx, TextEncoding::Utf8)
                .map_err(|e| ExecError::MalformedInstruction {
                    opcode,
                    reason: e.to_string(),
                })
        }
        CursorSlot::IndexRead(state) => {
            let payload = &state
                .current
                .as_ref()
                .ok_or(ExecError::MalformedInstruction {
                    opcode,
                    reason: "cursor has no current row".to_string(),
                })?
                .payload;
            state
                .header_cache
                .ensure(payload)
                .map_err(|e| ExecError::MalformedInstruction {
                    opcode,
                    reason: e.to_string(),
                })?;
            state
                .header_cache
                .column(payload, idx, TextEncoding::Utf8)
                .map_err(|e| ExecError::MalformedInstruction {
                    opcode,
                    reason: e.to_string(),
                })
        }
        _ => unreachable!("filtered to Table/IndexRead above"),
    }
}

/// `Column`: reads column `P2` of cursor `P1`'s current row into
/// register `P3`. A `NullRow`-forced table cursor always reads as NULL,
/// regardless of `P2`. Works on index-read cursors too (#444): real
/// SQLite reuses this same opcode against an index cursor's current
/// entry rather than defining a separate index-column opcode, so
/// covering-index scans and index-only aggregates decode straight out
/// of the index's own record via this path.
///
/// Known simplification: this does not substitute the rowid-alias
/// column (`INTEGER PRIMARY KEY`, stored as NULL in the record — see
/// `src/btree.rs`'s module doc) with the cursor's actual rowid; that
/// substitution is schema-aware and belongs to codegen (#91), which
/// knows which column, if any, is the alias and can emit `Rowid` instead
/// of `Column` for it.
pub fn column(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let idx = usize::try_from(instr.p2).map_err(|_| ExecError::MalformedInstruction {
        opcode: "Column",
        reason: format!("negative column index {}", instr.p2),
    })?;
    let value = read_row_column(vm, instr.p1, idx, "Column")?;
    vm.set_register(instr.p3, value)?;
    Ok(Step::Next)
}

/// `Rowid`: writes cursor `P1`'s current rowid into register `P2` (NULL
/// if the cursor is `NullRow`-forced).
pub fn rowid(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let value = match vm.cursor(instr.p1)? {
        CursorSlot::Table(state) => {
            if state.forced_null {
                Value::Null
            } else {
                let row = state
                    .current
                    .as_ref()
                    .ok_or(ExecError::MalformedInstruction {
                        opcode: "Rowid",
                        reason: "cursor has no current row".to_string(),
                    })?;
                Value::Integer(row.rowid)
            }
        }
        CursorSlot::EphemeralTable(state) => {
            let (rowid, _) = state.pos.and_then(|p| state.rows.get(p)).ok_or(
                ExecError::MalformedInstruction {
                    opcode: "Rowid",
                    reason: "cursor has no current row".to_string(),
                },
            )?;
            Value::Integer(*rowid)
        }
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode: "Rowid",
                slot: instr.p1,
                found: other.type_name(),
                expected: "table or ephemeral table cursor",
            })
        }
    };
    vm.set_register(instr.p2, value)?;
    Ok(Step::Next)
}

/// `SeekRowid`: positions cursor `P1` at the row whose rowid equals
/// register `P3`, jumping to `P2` if no such row exists.
pub fn seek_rowid(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let target = match vm.register(instr.p3)? {
        Value::Integer(i) => *i,
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "SeekRowid",
                reason: format!("target rowid register holds {other:?}, not an integer"),
            })
        }
    };
    let state = vm.table_cursor_mut(instr.p1, "SeekRowid")?;
    state.forced_null = false;
    let row = state
        .cursor
        .seek(target)
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "SeekRowid",
            reason: e.to_string(),
        })?;
    state.set_current(row);
    Ok(if state.current.is_none() {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `SeekIndexEq` (#243): probes index-read cursor `P1` (opened by
/// `OpenRead` with `P5` nonzero) for an exact match on the `P4::Int`
/// count of key columns starting at register `P3`, jumping to `P2` on a
/// miss. On a hit, decodes the matched index row's trailing rowid column
/// and records it in the cursor slot for a following `IdxRowid` — the
/// planner's join equality-index-selection fast path chains
/// `SeekIndexEq` + `IdxRowid` + `SeekRowid` (on the table cursor) in
/// place of an unconditional `Rewind`/`Next` full scan.
pub fn seek_index_eq(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = p4_count(instr, "SeekIndexEq")?;
    let probe = read_register_range(vm, instr.p3, count, "SeekIndexEq")?;
    let root_page = match vm.cursor(instr.p1)? {
        CursorSlot::IndexRead(state) => state.root_page,
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode: "SeekIndexEq",
                slot: instr.p1,
                found: other.type_name(),
                expected: "index read cursor",
            })
        }
    };
    let db = vm.db()?;
    let encoding = db.header.text_encoding;
    let usable_size = db.header.usable_page_size();
    // A fresh, one-shot `IndexCursor` rather than the slot's own
    // persisted traversal cursor: `SeekIndexEq` is a point lookup (#243),
    // unrelated to the sequential position `IdxRewind`/`IdxNext`/etc.
    // (#296) maintain in `state.cursor` for an index-ordered scan.
    let mut cursor = IndexCursor::new(Rc::clone(&db.source), usable_size, root_page);
    let found = cursor
        .seek(&probe, encoding)
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "SeekIndexEq",
            reason: e.to_string(),
        })?;
    let matched = match &found {
        Some(row) => {
            let key = decode_record(&row.payload, encoding).map_err(|e| {
                ExecError::MalformedInstruction {
                    opcode: "SeekIndexEq",
                    reason: e.to_string(),
                }
            })?;
            key.len() > probe.len()
                && key
                    .iter()
                    .zip(probe.iter())
                    .all(|(k, p)| compare(k, p, Collation::Binary).is_eq())
        }
        None => false,
    };
    let current = if matched { found } else { None };
    match vm.cursor_mut(instr.p1)? {
        CursorSlot::IndexRead(state) => state.set_current(current.clone()),
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode: "SeekIndexEq",
                slot: instr.p1,
                found: other.type_name(),
                expected: "index read cursor",
            })
        }
    }
    Ok(if current.is_none() {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// Decodes index-read cursor `P1`'s current row (an ordinary secondary
/// index entry — its decoded record's trailing column is always the
/// referenced table's rowid) into an `i64` rowid, for [`idx_rowid`] and
/// the `IdxRewind`/`IdxLast`/`IdxNext`/`IdxPrev` scan opcodes' shared
/// "have we got a row, and what's its rowid" question.
fn index_read_current_rowid(
    vm: &Vm,
    slot: i32,
    opcode: &'static str,
) -> Result<Option<i64>, ExecError> {
    let current = match vm.cursor(slot)? {
        CursorSlot::IndexRead(state) => &state.current,
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode,
                slot,
                found: other.type_name(),
                expected: "index read cursor",
            })
        }
    };
    let Some(row) = current else {
        return Ok(None);
    };
    let encoding = vm.db()?.header.text_encoding;
    let key =
        decode_record(&row.payload, encoding).map_err(|e| ExecError::MalformedInstruction {
            opcode,
            reason: e.to_string(),
        })?;
    match key.last() {
        Some(Value::Integer(rowid)) => Ok(Some(*rowid)),
        other => Err(ExecError::MalformedInstruction {
            opcode,
            reason: format!("index row's trailing rowid column is {other:?}"),
        }),
    }
}

fn index_read_state_mut<'a>(
    vm: &'a mut Vm,
    slot: i32,
    opcode: &'static str,
) -> Result<&'a mut IndexReadState, ExecError> {
    match vm.cursor_mut(slot)? {
        CursorSlot::IndexRead(state) => Ok(state),
        other => Err(ExecError::CursorTypeMismatch {
            opcode,
            slot,
            found: other.type_name(),
            expected: "index read cursor",
        }),
    }
}

/// `IdxRewind` (#296): positions index-read cursor `P1` (opened by
/// `OpenRead` with `P5` nonzero) at its first entry in ascending key
/// order, jumping to `P2` if the index is empty — the index-cursor
/// counterpart to `Rewind`, used by an index-ordered scan walking a
/// matching index forward.
pub fn idx_rewind(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = index_read_state_mut(vm, instr.p1, "IdxRewind")?;
    let row = state
        .cursor
        .first()
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "IdxRewind",
            reason: e.to_string(),
        })?;
    state.set_current(row);
    Ok(if state.current.is_some() {
        Step::Next
    } else {
        Step::Jump(to_pc(instr.p2))
    })
}

/// `IdxLast` (#296): positions index-read cursor `P1` at its last entry
/// (descending key order from here on), jumping to `P2` if the index is
/// empty — the index-cursor counterpart to `Last`, used by an
/// index-ordered scan walking a matching index backward (`ORDER BY ...
/// DESC` over an ascending index, or vice versa).
pub fn idx_last(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = index_read_state_mut(vm, instr.p1, "IdxLast")?;
    let row = state
        .cursor
        .last()
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "IdxLast",
            reason: e.to_string(),
        })?;
    state.set_current(row);
    Ok(if state.current.is_some() {
        Step::Next
    } else {
        Step::Jump(to_pc(instr.p2))
    })
}

/// `IdxNext` (#296): advances index-read cursor `P1` forward, jumping to
/// `P2` (typically back to the loop body's start) if another entry was
/// found — falls through once exhausted. Mirrors `Next`'s jump-on-found
/// shape; pairs with `IdxRewind`.
pub fn idx_next(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = index_read_state_mut(vm, instr.p1, "IdxNext")?;
    let row = state
        .cursor
        .next()
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "IdxNext",
            reason: e.to_string(),
        })?;
    state.set_current(row);
    Ok(if state.current.is_some() {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `IdxPrev` (#296): advances index-read cursor `P1` backward, jumping to
/// `P2` if another entry was found — falls through once exhausted. Pairs
/// with `IdxLast`, the same way `IdxNext` pairs with `IdxRewind`.
pub fn idx_prev(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = index_read_state_mut(vm, instr.p1, "IdxPrev")?;
    let row = state
        .cursor
        .prev()
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "IdxPrev",
            reason: e.to_string(),
        })?;
    state.set_current(row);
    Ok(if state.current.is_some() {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `IdxRowid` (#243): writes index-read cursor `P1`'s most recently
/// `SeekIndexEq`-matched trailing rowid into register `P2`. Errors if
/// called without a preceding successful `SeekIndexEq` on this cursor.
pub fn idx_rowid(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let rowid = index_read_current_rowid(vm, instr.p1, "IdxRowid")?.ok_or_else(|| {
        ExecError::MalformedInstruction {
            opcode: "IdxRowid",
            reason: "no current row on this index cursor (SeekIndexEq missed, or no \
                         positioning opcode was run)"
                .to_string(),
        }
    })?;
    vm.set_register(instr.p2, Value::Integer(rowid))?;
    Ok(Step::Next)
}

/// `NullRow`: forces cursor `P1` to read as an all-NULL row until its
/// next real positioning (`Rewind`/`Last`/`Next`/`SeekRowid`).
pub fn null_row(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let state = vm.table_cursor_mut(instr.p1, "NullRow")?;
    state.forced_null = true;
    state.set_current(None);
    Ok(Step::Next)
}

/// `Sequence`: writes ephemeral cursor `P1`'s next monotonic counter
/// value into register `P2` (independent of the dedup key — used to
/// allocate a synthetic rowid for an ephemeral-table row).
pub fn sequence(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let value = match vm.cursor_mut(instr.p1)? {
        CursorSlot::Ephemeral(state) => {
            let v = state.sequence;
            state.sequence = state.sequence.saturating_add(1);
            v
        }
        CursorSlot::EphemeralTable(state) => {
            let v = state.sequence;
            state.sequence = state.sequence.saturating_add(1);
            v
        }
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode: "Sequence",
                slot: instr.p1,
                found: other.type_name(),
                expected: "ephemeral or ephemeral table cursor",
            })
        }
    };
    vm.set_register(instr.p2, Value::Integer(value))?;
    Ok(Step::Next)
}

/// `Found`: probes ephemeral cursor `P1` for the key built from `P4`
/// (`Int`, the key column count) registers starting at `P3`, jumping to
/// `P2` if present. Either way, remembers the probed key as the target
/// of a following `IdxInsert`/`Delete`.
pub fn found(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = p4_count(instr, "Found")?;
    let values = read_register_range(vm, instr.p3, count, "Found")?;
    let key = encode_record(&values, TextEncoding::Utf8);
    let state = vm.ephemeral_mut(instr.p1, "Found")?;
    let present = state.entries.contains_key(&key);
    state.last_key = Some(key);
    Ok(if present {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `IdxInsert`: for an ephemeral cursor (DISTINCT's dedup path,
/// unchanged), inserts the key built from `P4` (`Int`, the key column
/// count) registers starting at `P2` into ephemeral cursor `P1`. For a
/// real [`CursorSlot::IndexWrite`] cursor (#194, opened by `OpenWrite`
/// with `P5` nonzero), instead encodes the same register range as a
/// full index entry and writes it into the on-disk index b-tree via
/// [`btree::insert_entry`] — `Err(BtreeError::DuplicateKey)` surfaces as
/// a `MalformedInstruction` (this opcode does not model `OR IGNORE`/`OR
/// REPLACE` conflict resolution).
pub fn idx_insert(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = p4_count(instr, "IdxInsert")?;
    let values = read_register_range(vm, instr.p2, count, "IdxInsert")?;
    match vm.cursor(instr.p1)? {
        CursorSlot::IndexWrite { root_page } => {
            let root_page = *root_page;
            let pager = vm.writer("IdxInsert")?;
            let db = vm.db()?;
            let encoding = db.header.text_encoding;
            let header = db.header;
            let mut pager = pager.borrow_mut();
            btree::insert_entry(&mut pager, &header, root_page, &values, encoding).map_err(
                |e| ExecError::MalformedInstruction {
                    opcode: "IdxInsert",
                    reason: e.to_string(),
                },
            )?;
            Ok(Step::Next)
        }
        CursorSlot::Ephemeral(_) => {
            let key = encode_record(&values, TextEncoding::Utf8);
            let state = vm.ephemeral_mut(instr.p1, "IdxInsert")?;
            if !state.entries.contains_key(&key) && state.entries.len() >= MAX_EPHEMERAL_ROWS {
                return Err(ExecError::EphemeralRowLimitExceeded {
                    opcode: "IdxInsert",
                    limit: MAX_EPHEMERAL_ROWS,
                });
            }
            state.entries.insert(key.clone(), values);
            state.last_key = Some(key);
            Ok(Step::Next)
        }
        other => Err(ExecError::CursorTypeMismatch {
            opcode: "IdxInsert",
            slot: instr.p1,
            found: other.type_name(),
            expected: "ephemeral or index write cursor",
        }),
    }
}

/// `IdxDelete` (#210): for an ephemeral cursor, removes the key built
/// from `P4` (`Int`, the key column count) registers starting at `P2`
/// from ephemeral cursor `P1`. For a real [`CursorSlot::IndexWrite`]
/// cursor, encodes the same register range as an index key and removes
/// the matching entry from the on-disk index b-tree via
/// [`btree::delete_entry`] — `Err(BtreeError::KeyNotFound)`
/// surfaces as a `MalformedInstruction`. Mirrors [`idx_insert`]'s operand
/// shape and cursor-kind dispatch.
pub fn idx_delete(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = p4_count(instr, "IdxDelete")?;
    let values = read_register_range(vm, instr.p2, count, "IdxDelete")?;
    match vm.cursor(instr.p1)? {
        CursorSlot::IndexWrite { root_page } => {
            let root_page = *root_page;
            let pager = vm.writer("IdxDelete")?;
            let db = vm.db()?;
            let encoding = db.header.text_encoding;
            let header = db.header;
            let mut pager = pager.borrow_mut();
            btree::delete_entry(&mut pager, &header, root_page, &values, encoding).map_err(
                |e| ExecError::MalformedInstruction {
                    opcode: "IdxDelete",
                    reason: e.to_string(),
                },
            )?;
            Ok(Step::Next)
        }
        CursorSlot::Ephemeral(_) => {
            let key = encode_record(&values, TextEncoding::Utf8);
            let state = vm.ephemeral_mut(instr.p1, "IdxDelete")?;
            state.entries.remove(&key);
            if state.last_key.as_ref() == Some(&key) {
                state.last_key = None;
            }
            Ok(Step::Next)
        }
        other => Err(ExecError::CursorTypeMismatch {
            opcode: "IdxDelete",
            slot: instr.p1,
            found: other.type_name(),
            expected: "ephemeral or index write cursor",
        }),
    }
}

/// `NoConflict` (#207): jumps to `P2` when no entry in the real index
/// b-tree rooted at cursor `P1`'s `IndexWrite` root page has a key whose
/// leading columns equal the `P4` (`Int`, key column count) registers
/// starting at `P3` — i.e. "no conflicting row exists for this
/// candidate UNIQUE key", the seek+branch primitive #207's own doc
/// (`src/vdbe/cursor.rs`, this comment) identified as missing. Falls
/// through (does not jump) when a matching entry IS found, so callers
/// emit their `ON CONFLICT` handling as the fallthrough body — mirroring
/// `SeekRowid`'s "jump on absence" shape used by the rowid-PK conflict
/// check in `src/codegen/insert.rs`.
///
/// On a conflict (fallthrough), also writes the conflicting entry's
/// trailing rowid column into register `P3 + count` — one past the
/// probe range — so an `OR REPLACE` caller can `SeekRowid` the table
/// cursor onto the row being displaced without a second index lookup.
/// Callers that don't need `OR REPLACE` may leave that register
/// unallocated for anything else, but MUST NOT reuse it for the probe
/// itself.
///
/// Built on [`IndexCursor::seek`] (`src/btree/index.rs`), a linear scan
/// from the first entry (BINARY collation only, Tier 0 scope — matches
/// the cursor's own documented limitation). The probe key is just the
/// index's declared columns, without the trailing rowid every on-disk
/// entry carries; `seek` returns the first entry whose full key is not
/// less than that shorter probe (`compare_keys`' `zip` naturally treats
/// the probe as a prefix), so this checks only that returned entry's own
/// leading columns for an exact match — the trailing rowid is irrelevant
/// to uniqueness.
pub fn no_conflict(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = p4_count(instr, "NoConflict")?;
    let probe = read_register_range(vm, instr.p3, count, "NoConflict")?;
    let root_page = match vm.cursor(instr.p1)? {
        CursorSlot::IndexWrite { root_page } => *root_page,
        other => {
            return Err(ExecError::CursorTypeMismatch {
                opcode: "NoConflict",
                slot: instr.p1,
                found: other.type_name(),
                expected: "index write cursor",
            })
        }
    };
    let db = vm.db()?;
    let encoding = db.header.text_encoding;
    let usable_size = db.header.usable_page_size();
    let mut cursor = IndexCursor::new(Rc::clone(&db.source), usable_size, root_page);
    let found = cursor
        .seek(&probe, encoding)
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "NoConflict",
            reason: e.to_string(),
        })?;
    let conflict_key = match found {
        Some(row) => {
            let key = decode_record(&row.payload, encoding).map_err(|e| {
                ExecError::MalformedInstruction {
                    opcode: "NoConflict",
                    reason: e.to_string(),
                }
            })?;
            let matches = key.len() >= probe.len()
                && key
                    .iter()
                    .zip(probe.iter())
                    .all(|(k, p)| compare(k, p, Collation::Binary).is_eq());
            matches.then_some(key)
        }
        None => None,
    };
    match conflict_key {
        Some(key) => {
            if let Some(rowid) = key.last() {
                let dest = instr.p3.saturating_add(i32::try_from(count).map_err(|_| {
                    ExecError::RegisterRangeTooLarge {
                        opcode: "NoConflict",
                        count: count as i32,
                    }
                })?);
                vm.set_register(dest, rowid.clone())?;
            }
            Ok(Step::Next)
        }
        None => Ok(Step::Jump(to_pc(instr.p2))),
    }
}

/// `IdxLE`: jumps to `P2` if the key built from `P4` (`Int`, the key
/// column count) registers starting at `P3` is `<=` ephemeral cursor
/// `P1`'s most recently probed/inserted key (byte-order comparison of
/// the encoded key, matching a BINARY-collated index).
///
/// Known scope limitation: the harvested use of this opcode
/// (`tools/opcodes-v2.json`) ties it to an `ORDER BY ... LIMIT 1`
/// index-seek query-planner optimization this ticket does not
/// implement (the full sorter path, Requirement 9, is what V2 actually
/// executes for `ORDER BY`) — there is no harvested example with more
/// than one occurrence to derive a fuller semantics from. This
/// implementation gives `IdxLE` a well-defined, testable meaning against
/// the same ephemeral cursor `Found`/`IdxInsert` already use, rather
/// than leaving it unimplemented, but does not claim oracle-exact parity
/// for the optimization the harvest actually observed it in.
pub fn idx_le(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let count = p4_count(instr, "IdxLE")?;
    let values = read_register_range(vm, instr.p3, count, "IdxLE")?;
    let probe = encode_record(&values, TextEncoding::Utf8);
    let state = vm.ephemeral_mut(instr.p1, "IdxLE")?;
    let holds = match &state.last_key {
        Some(key) => *key <= probe,
        None => true,
    };
    Ok(if holds {
        Step::Jump(to_pc(instr.p2))
    } else {
        Step::Next
    })
}

/// `Delete`: for an ephemeral cursor (unchanged), removes cursor `P1`'s
/// most recently probed/inserted entry (per `Found`/`IdxInsert`'s
/// `last_key`) — DISTINCT's "insert then delete the just-produced
/// duplicate" path (spec 009 Requirement 4). For a real
/// [`CursorSlot::Table`] write cursor (#194), deletes the row at the
/// cursor's *current* position (whatever `Rewind`/`Next`/`SeekRowid`
/// last positioned it on) from the on-disk table b-tree via
/// [`btree::delete_row`].
pub fn delete(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    match vm.cursor(instr.p1)? {
        CursorSlot::Table(state) => {
            let rowid = state
                .current
                .as_ref()
                .ok_or(ExecError::MalformedInstruction {
                    opcode: "Delete",
                    reason: "cursor has no current row".to_string(),
                })?
                .rowid;
            let root_page = state.root_page;
            let pager = vm.writer("Delete")?;
            let db = vm.db()?;
            let header = db.header;
            let mut pager = pager.borrow_mut();
            btree::delete_row(&mut pager, &header, root_page, rowid).map_err(|e| {
                ExecError::MalformedInstruction {
                    opcode: "Delete",
                    reason: e.to_string(),
                }
            })?;
            drop(pager);
            // The row this cursor was positioned on is now gone —
            // clear `current` so a stray follow-up `Rowid`/`Column`
            // reads as "no row" rather than stale data.
            if let CursorSlot::Table(state) = vm.cursor_mut(instr.p1)? {
                state.set_current(None);
            }
            Ok(Step::Next)
        }
        CursorSlot::Ephemeral(_) => {
            let state = vm.ephemeral_mut(instr.p1, "Delete")?;
            if let Some(key) = state.last_key.take() {
                state.entries.remove(&key);
            }
            Ok(Step::Next)
        }
        other => Err(ExecError::CursorTypeMismatch {
            opcode: "Delete",
            slot: instr.p1,
            found: other.type_name(),
            expected: "ephemeral or table cursor",
        }),
    }
}

/// `Insert` (#194): inserts a row into the table b-tree cursor `P1` is
/// open on (must be a real [`CursorSlot::Table`] write cursor, opened via
/// `OpenWrite`), OR (#257) appends a row into an in-memory
/// [`CursorSlot::EphemeralTable`] (opened by `OpenEphemeral` with `P5`
/// nonzero) — used to materialize a subquery-in-FROM. Either way, `P2`
/// holds the row's rowid (an integer register) and `P3` holds the
/// already-`MakeRecord`-encoded payload blob. The real-table path
/// delegates to [`btree::insert_row`]; `OR REPLACE`/`OR IGNORE`-style
/// `P5` conflict-resolution flags are not modeled there — every insert is
/// an unconditional add, matching `insert_row`'s own contract.
pub fn insert(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let rowid = match vm.register(instr.p2)? {
        Value::Integer(i) => *i,
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "Insert",
                reason: format!("rowid register holds {other:?}, not an integer"),
            })
        }
    };
    let payload = match vm.register(instr.p3)? {
        Value::Blob(bytes) => bytes.clone(),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "Insert",
                reason: format!("record register holds {other:?}, not a blob"),
            })
        }
    };
    match vm.cursor(instr.p1)? {
        CursorSlot::EphemeralTable(_) => {
            // No real db is attached for a purely in-memory VM (e.g. the
            // ephemeral-table unit tests below) — fall back to UTF-8,
            // matching every other decode site's default.
            let encoding = vm
                .db()
                .map(|db| db.header.text_encoding)
                .unwrap_or(TextEncoding::Utf8);
            let values =
                decode_record(&payload, encoding).map_err(|e| ExecError::MalformedInstruction {
                    opcode: "Insert",
                    reason: e.to_string(),
                })?;
            let state = vm.ephemeral_table_mut(instr.p1, "Insert")?;
            if state.rows.len() >= MAX_EPHEMERAL_ROWS {
                return Err(ExecError::EphemeralRowLimitExceeded {
                    opcode: "Insert",
                    limit: MAX_EPHEMERAL_ROWS,
                });
            }
            state.rows.push((rowid, values));
            Ok(Step::Next)
        }
        _ => {
            let root_page = vm.table_cursor_mut(instr.p1, "Insert")?.root_page;
            let pager = vm.writer("Insert")?;
            let db = vm.db()?;
            let header = db.header;
            let mut pager = pager.borrow_mut();
            btree::insert_row(&mut pager, &header, root_page, rowid, &payload).map_err(|e| {
                ExecError::MalformedInstruction {
                    opcode: "Insert",
                    reason: e.to_string(),
                }
            })?;
            Ok(Step::Next)
        }
    }
}

/// `NewRowid` (#194): computes a fresh rowid for table cursor `P1`
/// (`max(rowid) + 1`, or `1` for an empty table — via
/// [`TableCursor::last`]) and writes it to register `P2`.
///
/// AUTOINCREMENT simplification: this VDBE layer has no schema-aware
/// way to know whether a table was declared `INTEGER PRIMARY KEY
/// AUTOINCREMENT` (that bit lives in codegen/the schema, not here), so
/// AUTOINCREMENT handling is opt-in per instruction instead: when `P5`
/// is nonzero AND `P4` carries the table's name (`P4::Str`), this also
/// consults/bumps `sqlite_sequence` via
/// [`crate::btree::ensure_sqlite_sequence_table`]/[`crate::btree::update_sequence`],
/// taking `max(sqlite_sequence.seq, TableCursor::last() rowid) + 1`
/// (matching stock SQLite: `sqlite_sequence` never regresses even after
/// the row it recorded is deleted). Without `P5`/`P4`, this opcode is
/// plain non-AUTOINCREMENT rowid allocation.
pub fn new_rowid(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let root_page = vm.table_cursor_mut(instr.p1, "NewRowid")?.root_page;
    let db = vm.db()?;
    let mut probe = TableCursor::new(Rc::clone(&db.source), &db.header, root_page);
    let max_from_table = probe
        .last()
        .map_err(|e| ExecError::MalformedInstruction {
            opcode: "NewRowid",
            reason: e.to_string(),
        })?
        .map_or(0, |row| row.rowid);

    let new_rowid = if instr.p5 != 0 {
        let table_name = match &instr.p4 {
            P4::Str(name) => name.clone(),
            other => {
                return Err(ExecError::MalformedInstruction {
                    opcode: "NewRowid",
                    reason: format!(
                        "AUTOINCREMENT requested (P5 nonzero) but P4 is not a table-name string, got {other:?}"
                    ),
                })
            }
        };
        let pager = vm.writer("NewRowid")?;
        let db = vm.db()?;
        let header = db.header;
        let mut pager = pager.borrow_mut();
        let seq_root = btree::ensure_sqlite_sequence_table(&mut pager, &header).map_err(|e| {
            ExecError::MalformedInstruction {
                opcode: "NewRowid",
                reason: e.to_string(),
            }
        })?;
        let mut seq_cursor = TableCursor::new(&*pager, &header, seq_root);
        let mut tracked_seq = 0i64;
        let mut row = seq_cursor
            .first()
            .map_err(|e| ExecError::MalformedInstruction {
                opcode: "NewRowid",
                reason: e.to_string(),
            })?;
        while let Some(r) = row {
            let values = decode_record(&r.payload, header.text_encoding).map_err(|e| {
                ExecError::MalformedInstruction {
                    opcode: "NewRowid",
                    reason: e.to_string(),
                }
            })?;
            if let (Some(Value::Text(n)), Some(Value::Integer(seq))) =
                (values.first(), values.get(1))
            {
                if *n == table_name.clone().into() {
                    tracked_seq = *seq;
                    break;
                }
            }
            row = seq_cursor
                .next()
                .map_err(|e| ExecError::MalformedInstruction {
                    opcode: "NewRowid",
                    reason: e.to_string(),
                })?;
        }
        let candidate = max_from_table.max(tracked_seq).saturating_add(1);
        btree::update_sequence(&mut pager, &header, &table_name, candidate).map_err(|e| {
            ExecError::MalformedInstruction {
                opcode: "NewRowid",
                reason: e.to_string(),
            }
        })?;
        candidate
    } else {
        max_from_table.saturating_add(1)
    };

    vm.set_register(instr.p2, Value::Integer(new_rowid))?;
    Ok(Step::Next)
}

/// `CreateTable` (#215): allocates a fresh table-b-tree root page,
/// registers it in `sqlite_master`, and bumps the schema cookie — the
/// whole statement in one opcode, per `codegen::create_table`'s doc.
pub fn create_table(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let (name, sql) = match &instr.p4 {
        P4::CreateTable { name, sql } => (name.clone(), sql.clone()),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "CreateTable",
                reason: format!("expected P4::CreateTable, got {other:?}"),
            })
        }
    };
    let pager = vm.writer("CreateTable")?;
    let db = vm.db()?;
    let header = db.header;
    let mut pager = pager.borrow_mut();
    let root_page = btree::create_empty_table_root(&mut pager).map_err(|e| {
        ExecError::MalformedInstruction {
            opcode: "CreateTable",
            reason: e.to_string(),
        }
    })?;
    btree::insert_master_row(
        &mut pager,
        &header,
        &btree::MasterEntry {
            kind: "table".to_string(),
            name: name.clone(),
            tbl_name: name,
            rootpage: root_page,
            sql,
        },
    )
    .map_err(|e| ExecError::MalformedInstruction {
        opcode: "CreateTable",
        reason: e.to_string(),
    })?;
    btree::bump_schema_cookie(&mut pager).map_err(|e| ExecError::MalformedInstruction {
        opcode: "CreateTable",
        reason: e.to_string(),
    })?;
    Ok(Step::Next)
}

/// `CreateView` (#380): registers a `sqlite_master` row with
/// `type = 'view'` and `rootpage = 0` — a view has no b-tree of its own,
/// so unlike [`create_table`] this never allocates a root page.
pub fn create_view(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let (name, sql) = match &instr.p4 {
        P4::CreateView { name, sql } => (name.clone(), sql.clone()),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "CreateView",
                reason: format!("expected P4::CreateView, got {other:?}"),
            })
        }
    };
    let pager = vm.writer("CreateView")?;
    let db = vm.db()?;
    let header = db.header;
    let mut pager = pager.borrow_mut();
    btree::insert_master_row(
        &mut pager,
        &header,
        &btree::MasterEntry {
            kind: "view".to_string(),
            name: name.clone(),
            tbl_name: name,
            rootpage: 0,
            sql,
        },
    )
    .map_err(|e| ExecError::MalformedInstruction {
        opcode: "CreateView",
        reason: e.to_string(),
    })?;
    btree::bump_schema_cookie(&mut pager).map_err(|e| ExecError::MalformedInstruction {
        opcode: "CreateView",
        reason: e.to_string(),
    })?;
    Ok(Step::Next)
}

/// `DropTable` (#215): frees the target table's b-tree pages plus every
/// index on it (cascading), removes the corresponding `sqlite_master`
/// rows, and bumps the schema cookie once.
pub fn drop_table(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let (name, root_page, indexes) = match &instr.p4 {
        P4::DropTable {
            name,
            root_page,
            indexes,
        } => (name.clone(), *root_page, indexes.clone()),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "DropTable",
                reason: format!("expected P4::DropTable, got {other:?}"),
            })
        }
    };
    let pager = vm.writer("DropTable")?;
    let db = vm.db()?;
    let header = db.header;
    let mut pager = pager.borrow_mut();
    for (index_name, index_root) in &indexes {
        btree::free_btree_pages(&mut pager, &header, *index_root).map_err(|e| {
            ExecError::MalformedInstruction {
                opcode: "DropTable",
                reason: e.to_string(),
            }
        })?;
        btree::delete_master_row(&mut pager, &header, index_name).map_err(|e| {
            ExecError::MalformedInstruction {
                opcode: "DropTable",
                reason: e.to_string(),
            }
        })?;
    }
    btree::free_btree_pages(&mut pager, &header, root_page).map_err(|e| {
        ExecError::MalformedInstruction {
            opcode: "DropTable",
            reason: e.to_string(),
        }
    })?;
    btree::delete_master_row(&mut pager, &header, &name).map_err(|e| {
        ExecError::MalformedInstruction {
            opcode: "DropTable",
            reason: e.to_string(),
        }
    })?;
    btree::bump_schema_cookie(&mut pager).map_err(|e| ExecError::MalformedInstruction {
        opcode: "DropTable",
        reason: e.to_string(),
    })?;
    Ok(Step::Next)
}

/// `CreateIndex` (#215): allocates a fresh index-b-tree root page,
/// populates it with one entry per pre-existing row of the target table
/// (see `btree::populate_index_from_table`), registers the index in
/// `sqlite_master`, and bumps the schema cookie.
pub fn create_index(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let (name, table_name, table_root_page, sql, column_indices) = match &instr.p4 {
        P4::CreateIndex {
            name,
            table_name,
            table_root_page,
            sql,
            column_indices,
            ..
        } => (
            name.clone(),
            table_name.clone(),
            *table_root_page,
            sql.clone(),
            column_indices.clone(),
        ),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "CreateIndex",
                reason: format!("expected P4::CreateIndex, got {other:?}"),
            })
        }
    };
    let pager = vm.writer("CreateIndex")?;
    let db = vm.db()?;
    let header = db.header;
    let mut pager = pager.borrow_mut();
    let index_root = btree::create_empty_index_root(&mut pager).map_err(|e| {
        ExecError::MalformedInstruction {
            opcode: "CreateIndex",
            reason: e.to_string(),
        }
    })?;
    btree::populate_index_from_table(
        &mut pager,
        &header,
        table_root_page,
        index_root,
        &column_indices,
    )
    .map_err(|e| ExecError::MalformedInstruction {
        opcode: "CreateIndex",
        reason: e.to_string(),
    })?;
    btree::insert_master_row(
        &mut pager,
        &header,
        &btree::MasterEntry {
            kind: "index".to_string(),
            name: name.clone(),
            tbl_name: table_name,
            rootpage: index_root,
            sql,
        },
    )
    .map_err(|e| ExecError::MalformedInstruction {
        opcode: "CreateIndex",
        reason: e.to_string(),
    })?;
    btree::bump_schema_cookie(&mut pager).map_err(|e| ExecError::MalformedInstruction {
        opcode: "CreateIndex",
        reason: e.to_string(),
    })?;
    Ok(Step::Next)
}

/// `DropIndex` (#215): frees the target index's b-tree pages, removes
/// its `sqlite_master` row, and bumps the schema cookie.
pub fn drop_index(vm: &mut Vm, instr: &Instruction) -> Result<Step, ExecError> {
    let (name, root_page) = match &instr.p4 {
        P4::DropIndex { name, root_page } => (name.clone(), *root_page),
        other => {
            return Err(ExecError::MalformedInstruction {
                opcode: "DropIndex",
                reason: format!("expected P4::DropIndex, got {other:?}"),
            })
        }
    };
    let pager = vm.writer("DropIndex")?;
    let db = vm.db()?;
    let header = db.header;
    let mut pager = pager.borrow_mut();
    btree::free_btree_pages(&mut pager, &header, root_page).map_err(|e| {
        ExecError::MalformedInstruction {
            opcode: "DropIndex",
            reason: e.to_string(),
        }
    })?;
    btree::delete_master_row(&mut pager, &header, &name).map_err(|e| {
        ExecError::MalformedInstruction {
            opcode: "DropIndex",
            reason: e.to_string(),
        }
    })?;
    btree::bump_schema_cookie(&mut pager).map_err(|e| ExecError::MalformedInstruction {
        opcode: "DropIndex",
        reason: e.to_string(),
    })?;
    Ok(Step::Next)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::header::DatabaseHeader;
    use crate::vdbe::affinity::Affinity;
    use crate::vdbe::program::Opcode;
    use crate::vfs::{UnixVfs, Vfs, VfsPageSource};
    use std::path::Path;

    fn open_vm(fixture: &str) -> Vm {
        let path = Path::new("tests/corpus/fixtures/btrees").join(fixture);
        let vfs = UnixVfs;
        let file = vfs.open_read(&path).unwrap();
        let mut header_buf = [0u8; 100];
        file.read_at(&mut header_buf, 0).unwrap();
        let header = DatabaseHeader::parse(&header_buf).unwrap();
        let source = VfsPageSource::open(&vfs, &path, header.page_size).unwrap();
        Vm::with_db(Rc::new(source), header)
    }

    /// A one-page, empty-leaf-root database (root page 1 doubling as a
    /// table b-tree root, rather than a real `sqlite_master` page — a
    /// simplification this ticket's tests share with
    /// `src/btree/insert.rs::tests::minimal_db`, whose private
    /// `write_leaf_page` helper isn't reachable from here). `page_type`
    /// is `0x0d` (`LEAF_TABLE`) or `0x0a` (`LEAF_INDEX`) — see
    /// `src/btree/index.rs`'s `LEAF_INDEX` constant.
    fn minimal_writable_db(
        page_size: u32,
        page_type: u8,
    ) -> (crate::vfs::MemoryVfs, DatabaseHeader) {
        let mut page1 = vec![0u8; page_size as usize];
        page1[0..16].copy_from_slice(b"SQLite format 3\0");
        page1[16..18].copy_from_slice(&u16::try_from(page_size).unwrap_or(1).to_be_bytes());
        page1[18] = 1;
        page1[19] = 1;
        page1[28..32].copy_from_slice(&1u32.to_be_bytes());
        page1[56..60].copy_from_slice(&1u32.to_be_bytes());

        let header_start = 100usize;
        page1[header_start] = page_type;
        page1[header_start + 1..header_start + 3].copy_from_slice(&0u16.to_be_bytes());
        page1[header_start + 3..header_start + 5].copy_from_slice(&0u16.to_be_bytes());
        let content_start = if page_size == 65536 {
            0u16
        } else {
            u16::try_from(page_size).unwrap()
        };
        page1[header_start + 5..header_start + 7].copy_from_slice(&content_start.to_be_bytes());
        page1[header_start + 7] = 0;

        let mut header_bytes = [0u8; 100];
        header_bytes.copy_from_slice(&page1[..100]);
        let header = DatabaseHeader::parse(&header_bytes).unwrap();

        let mut vfs = crate::vfs::MemoryVfs::new();
        vfs.insert("/test.db", page1);
        (vfs, header)
    }

    fn writable_vm(page_type: u8) -> Vm {
        let (vfs, header) = minimal_writable_db(512, page_type);
        let pager = crate::pager::Pager::open(&vfs, Path::new("/test.db"), 512).unwrap();
        Vm::with_writable_db(pager, header)
    }

    #[test]
    fn full_scan_opens_rewinds_iterates_reads() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();

        let step = rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 999, 0)).unwrap();
        assert_eq!(step, Step::Next);

        let mut rowids = Vec::new();
        loop {
            rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 10, 0)).unwrap();
            rowids.push(vm.register(10).unwrap().clone());
            column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 11)).unwrap();

            match next(&mut vm, &Instruction::new(Opcode::Next, 0, 1, 0)).unwrap() {
                Step::Jump(1) => continue,
                Step::Next => break,
                other => panic!("unexpected step {other:?}"),
            }
        }

        assert_eq!(rowids.len(), 3000);
        assert_eq!(rowids[0], Value::Integer(1));
        assert_eq!(rowids[2999], Value::Integer(3000));
        assert_eq!(
            *vm.register(11).unwrap(),
            Value::Text("row number 3000".to_string().into())
        );
    }

    #[test]
    fn seek_rowid_jumps_to_p2_when_the_target_rowid_is_absent() {
        let mut vm = open_vm("table_single_page.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        vm.set_register(5, Value::Integer(999)).unwrap();
        let step = seek_rowid(&mut vm, &Instruction::new(Opcode::SeekRowid, 0, 42, 5)).unwrap();
        assert_eq!(step, Step::Jump(42));
    }

    #[test]
    fn seek_rowid_skips_full_scan_on_pk_equality() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        vm.set_register(5, Value::Integer(1500)).unwrap();
        let step = seek_rowid(&mut vm, &Instruction::new(Opcode::SeekRowid, 0, 42, 5)).unwrap();
        assert_eq!(step, Step::Next);
        rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 10, 0)).unwrap();
        assert_eq!(*vm.register(10).unwrap(), Value::Integer(1500));
    }

    #[test]
    fn null_row_forces_all_null_reads_until_repositioned() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 999, 0)).unwrap();
        null_row(&mut vm, &Instruction::new(Opcode::NullRow, 0, 0, 0)).unwrap();

        column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 11)).unwrap();
        assert_eq!(*vm.register(11).unwrap(), Value::Null);
        rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 12, 0)).unwrap();
        assert_eq!(*vm.register(12).unwrap(), Value::Null);
    }

    #[test]
    fn distinct_probes_ephemeral_index_before_emit() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 1, 0)).unwrap();

        // Row "a": not found, insert, passes through.
        vm.set_register(0, Value::Text("a".to_string().into()))
            .unwrap();
        let found_a = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_a, Step::Next);
        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)),
        )
        .unwrap();

        // Row "a" again: found, discard (the DISTINCT dedup path).
        let found_a_again = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_a_again, Step::Jump(99));

        // Row "b": not found, insert, passes through.
        vm.set_register(0, Value::Text("b".to_string().into()))
            .unwrap();
        let found_b = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_b, Step::Next);
    }

    #[test]
    fn distinct_treats_two_nulls_as_equal_unlike_the_eq_operator() {
        // DISTINCT's ephemeral-index dedup is exact-byte record equality,
        // not SQL's `=` — two NULL rows collapse to one here (spec 008's
        // three-valued logic says `NULL = NULL` is UNKNOWN, never true;
        // spec 009 Requirement 9's ORDER BY default NULL placement is a
        // third, independent rule again). See spec 009 Requirement 9's
        // "NULL is comparison-distinct across `=`, DISTINCT, and ORDER BY"
        // scenario (#146).
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 1, 0)).unwrap();

        // Row NULL: not found, insert, passes through.
        vm.set_register(0, Value::Null).unwrap();
        let found_first_null = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_first_null, Step::Next);
        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)),
        )
        .unwrap();

        // Row NULL again: found, discard — NULL is equal to NULL for
        // DISTINCT's dedup, unlike `=`.
        vm.set_register(0, Value::Null).unwrap();
        let found_second_null = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_second_null, Step::Jump(99));
    }

    #[test]
    fn sequence_hands_out_a_monotonic_counter_independent_of_the_dedup_key() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 1, 0)).unwrap();
        sequence(&mut vm, &Instruction::new(Opcode::Sequence, 0, 5, 0)).unwrap();
        assert_eq!(*vm.register(5).unwrap(), Value::Integer(0));
        sequence(&mut vm, &Instruction::new(Opcode::Sequence, 0, 6, 0)).unwrap();
        assert_eq!(*vm.register(6).unwrap(), Value::Integer(1));
    }

    #[test]
    fn delete_removes_the_just_probed_duplicate_row() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 1, 0)).unwrap();
        vm.set_register(0, Value::Text("a".to_string().into()))
            .unwrap();
        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)),
        )
        .unwrap();
        found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        delete(&mut vm, &Instruction::new(Opcode::Delete, 0, 0, 0)).unwrap();

        let found_again = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_again, Step::Next);
    }

    #[test]
    fn cursor_type_mismatch_errors_instead_of_panicking() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 1, 0)).unwrap();
        let err = rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 5, 0)).unwrap_err();
        assert!(matches!(err, ExecError::CursorTypeMismatch { .. }));
    }

    #[test]
    fn unopened_cursor_slot_errors_instead_of_panicking() {
        let mut vm = Vm::new();
        let err = rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 5, 0)).unwrap_err();
        assert!(matches!(err, ExecError::CursorNotOpen { slot: 0 }));
    }

    // --- #194: write-path opcodes (OpenWrite/Insert/Delete/IdxInsert/NewRowid) ---

    #[test]
    fn open_write_requires_a_writable_vm() {
        // A read-only `Vm::with_db` must reject `OpenWrite` rather than
        // silently opening a cursor that later opcodes can't actually
        // write through.
        let mut vm = open_vm("table_multipage.db");
        let err = open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 2, 0)).unwrap_err();
        assert!(matches!(err, ExecError::NoDatabase { .. }));
    }

    #[test]
    fn new_rowid_starts_at_one_on_an_empty_table() {
        let mut vm = writable_vm(0x0d); // LEAF_TABLE
        open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 1, 0)).unwrap();
        new_rowid(&mut vm, &Instruction::new(Opcode::NewRowid, 0, 5, 0)).unwrap();
        assert_eq!(*vm.register(5).unwrap(), Value::Integer(1));
    }

    #[test]
    fn insert_then_read_back_round_trips_through_make_record_and_column() {
        let mut vm = writable_vm(0x0d); // LEAF_TABLE
        open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 1, 0)).unwrap();

        // NewRowid -> r0.
        new_rowid(&mut vm, &Instruction::new(Opcode::NewRowid, 0, 0, 0)).unwrap();
        assert_eq!(*vm.register(0).unwrap(), Value::Integer(1));

        // MakeRecord over r1..r3 (with INTEGER/TEXT affinity applied to
        // text-literal-but-numeric-looking input in r1) -> r3.
        vm.set_register(1, Value::Text("42".to_string().into()))
            .unwrap();
        vm.set_register(2, Value::Text("hello".to_string().into()))
            .unwrap();
        crate::vdbe::result::make_record(
            &mut vm,
            &Instruction::with_p4(
                Opcode::MakeRecord,
                1,
                2,
                3,
                P4::Affinity(vec![
                    Affinity::Integer.to_p4_byte(),
                    Affinity::Text.to_p4_byte(),
                ]),
            ),
        )
        .unwrap();
        assert!(matches!(vm.register(3).unwrap(), Value::Blob(_)));

        // Insert cursor 0, rowid r0, record r3.
        insert(&mut vm, &Instruction::new(Opcode::Insert, 0, 0, 3)).unwrap();

        // Read back through the same cursor's Rewind/Column — V1's
        // reader path (`decode_record`) must decode exactly what was
        // written, with the affinity-coerced INTEGER, not the original
        // TEXT "42".
        rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 999, 0)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 10)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 11)).unwrap();
        assert_eq!(*vm.register(10).unwrap(), Value::Integer(42));
        assert_eq!(
            *vm.register(11).unwrap(),
            Value::Text("hello".to_string().into())
        );
        rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 12, 0)).unwrap();
        assert_eq!(*vm.register(12).unwrap(), Value::Integer(1));
    }

    #[test]
    fn new_rowid_after_insert_skips_past_the_max_existing_rowid() {
        let mut vm = writable_vm(0x0d);
        open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 1, 0)).unwrap();
        vm.set_register(1, Value::Integer(7)).unwrap();
        crate::vdbe::result::make_record(&mut vm, &Instruction::new(Opcode::MakeRecord, 1, 1, 2))
            .unwrap();
        vm.set_register(0, Value::Integer(5)).unwrap();
        insert(&mut vm, &Instruction::new(Opcode::Insert, 0, 0, 2)).unwrap();

        new_rowid(&mut vm, &Instruction::new(Opcode::NewRowid, 0, 9, 0)).unwrap();
        assert_eq!(*vm.register(9).unwrap(), Value::Integer(6));
    }

    #[test]
    fn delete_removes_the_row_at_the_cursors_current_position() {
        let mut vm = writable_vm(0x0d);
        open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 1, 0)).unwrap();
        vm.set_register(1, Value::Integer(99)).unwrap();
        crate::vdbe::result::make_record(&mut vm, &Instruction::new(Opcode::MakeRecord, 1, 1, 2))
            .unwrap();
        vm.set_register(0, Value::Integer(1)).unwrap();
        insert(&mut vm, &Instruction::new(Opcode::Insert, 0, 0, 2)).unwrap();

        rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 999, 0)).unwrap();
        delete(&mut vm, &Instruction::new(Opcode::Delete, 0, 0, 0)).unwrap();

        let step = rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 999, 0)).unwrap();
        assert_eq!(step, Step::Jump(999));
    }

    #[test]
    fn new_rowid_autoincrement_consults_and_bumps_sqlite_sequence() {
        let mut vm = writable_vm(0x0d);
        open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 1, 0)).unwrap();

        let mut instr = Instruction::with_p4(Opcode::NewRowid, 0, 5, 0, P4::Str("t".to_string()));
        instr.p5 = 1;
        new_rowid(&mut vm, &instr).unwrap();
        assert_eq!(*vm.register(5).unwrap(), Value::Integer(1));

        // sqlite_sequence now tracks ("t", 1); a second NewRowid call
        // (simulating a second INSERT without actually inserting a row
        // in between, which this focused test doesn't need) must not
        // regress below the tracked value.
        new_rowid(&mut vm, &instr).unwrap();
        assert_eq!(*vm.register(5).unwrap(), Value::Integer(2));
    }

    #[test]
    fn idx_insert_real_cursor_writes_an_index_entry_readable_by_index_cursor() {
        let mut vm = writable_vm(0x0a); // LEAF_INDEX
        let mut open_instr = Instruction::new(Opcode::OpenWrite, 0, 1, 0);
        open_instr.p5 = 1; // nonzero P5: open a real index write cursor
        open_write(&mut vm, &open_instr).unwrap();

        vm.set_register(0, Value::Integer(5)).unwrap();
        vm.set_register(1, Value::Text("x".to_string().into()))
            .unwrap();
        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(2)),
        )
        .unwrap();

        let db = vm.db().unwrap();
        let mut index_cursor =
            crate::btree::IndexCursor::new(Rc::clone(&db.source), db.header.usable_page_size(), 1);
        let row = index_cursor.first().unwrap().unwrap();
        let values = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
        assert_eq!(
            values,
            vec![Value::Integer(5), Value::Text("x".to_string().into())]
        );
    }

    #[test]
    fn idx_delete_real_cursor_removes_an_index_entry() {
        let mut vm = writable_vm(0x0a); // LEAF_INDEX
        let mut open_instr = Instruction::new(Opcode::OpenWrite, 0, 1, 0);
        open_instr.p5 = 1; // nonzero P5: open a real index write cursor
        open_write(&mut vm, &open_instr).unwrap();

        vm.set_register(0, Value::Integer(5)).unwrap();
        vm.set_register(1, Value::Text("x".to_string().into()))
            .unwrap();
        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(2)),
        )
        .unwrap();

        idx_delete(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxDelete, 0, 0, 0, P4::Int(2)),
        )
        .unwrap();

        let db = vm.db().unwrap();
        let mut index_cursor =
            crate::btree::IndexCursor::new(Rc::clone(&db.source), db.header.usable_page_size(), 1);
        assert!(index_cursor.first().unwrap().is_none());
    }

    #[test]
    fn no_conflict_falls_through_and_reports_the_rowid_when_the_key_already_exists() {
        let mut vm = writable_vm(0x0a); // LEAF_INDEX
        let mut open_instr = Instruction::new(Opcode::OpenWrite, 0, 1, 0);
        open_instr.p5 = 1; // nonzero P5: open a real index write cursor
        open_write(&mut vm, &open_instr).unwrap();

        // One index entry: column value "v1", trailing rowid 42.
        vm.set_register(0, Value::Text("v1".to_string().into()))
            .unwrap();
        vm.set_register(1, Value::Integer(42)).unwrap();
        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(2)),
        )
        .unwrap();

        // Probe with just the column value (no trailing rowid) at
        // register 5, reserving register 6 for the conflicting rowid.
        vm.set_register(5, Value::Text("v1".to_string().into()))
            .unwrap();
        let step = no_conflict(
            &mut vm,
            &Instruction::with_p4(Opcode::NoConflict, 0, 999, 5, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(step, Step::Next, "a matching entry must not jump");
        assert_eq!(*vm.register(6).unwrap(), Value::Integer(42));
    }

    #[test]
    fn no_conflict_jumps_to_p2_when_no_matching_key_exists() {
        let mut vm = writable_vm(0x0a); // LEAF_INDEX
        let mut open_instr = Instruction::new(Opcode::OpenWrite, 0, 1, 0);
        open_instr.p5 = 1;
        open_write(&mut vm, &open_instr).unwrap();

        vm.set_register(0, Value::Text("v1".to_string().into()))
            .unwrap();
        vm.set_register(1, Value::Integer(42)).unwrap();
        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(2)),
        )
        .unwrap();

        vm.set_register(5, Value::Text("v2".to_string().into()))
            .unwrap();
        let step = no_conflict(
            &mut vm,
            &Instruction::with_p4(Opcode::NoConflict, 0, 999, 5, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(step, Step::Jump(999));
    }

    #[test]
    fn idx_delete_ephemeral_cursor_removes_the_entry() {
        let mut vm = writable_vm(0x0d);
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 0, 0)).unwrap();

        vm.set_register(0, Value::Integer(5)).unwrap();
        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)),
        )
        .unwrap();
        let found_step = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 999, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(found_step, Step::Jump(999));

        idx_delete(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxDelete, 0, 0, 0, P4::Int(1)),
        )
        .unwrap();

        let step = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 999, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(step, Step::Next);
    }

    // --- additional coverage: error branches, Last, OpenPseudo, IdxLE,
    // CreateTable/DropTable/CreateIndex/DropIndex, type_name(). ---

    #[test]
    fn cursor_slot_type_name_reports_every_variant() {
        assert_eq!(
            CursorSlot::IndexWrite { root_page: 1 }.type_name(),
            "index write cursor"
        );
        assert_eq!(
            CursorSlot::Pseudo { register: 0 }.type_name(),
            "pseudo cursor"
        );
    }

    #[test]
    fn last_positions_at_the_highest_rowid_and_jumps_when_empty() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        let step = last(&mut vm, &Instruction::new(Opcode::Last, 0, 999, 0)).unwrap();
        assert_eq!(step, Step::Next);
        rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 10, 0)).unwrap();
        assert_eq!(*vm.register(10).unwrap(), Value::Integer(3000));
    }

    #[test]
    fn last_jumps_to_p2_on_an_empty_table() {
        let mut vm = writable_vm(0x0d);
        open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 1, 0)).unwrap();
        let step = last(&mut vm, &Instruction::new(Opcode::Last, 0, 999, 0)).unwrap();
        assert_eq!(step, Step::Jump(999));
    }

    #[test]
    fn ephemeral_type_mismatch_errors_instead_of_panicking() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        let err = sequence(&mut vm, &Instruction::new(Opcode::Sequence, 0, 5, 0)).unwrap_err();
        assert!(matches!(err, ExecError::CursorTypeMismatch { .. }));
    }

    #[test]
    fn p4_count_rejects_a_negative_int_and_a_non_int_p4() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 0, 0)).unwrap();
        let err = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Int(-1)),
        )
        .unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));

        let err = found(
            &mut vm,
            &Instruction::with_p4(Opcode::Found, 0, 99, 0, P4::Bool(true)),
        )
        .unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn column_reads_through_an_open_pseudo_cursor() {
        let mut vm = Vm::new();
        vm.set_register(3, Value::Integer(7)).unwrap();
        vm.set_register(4, Value::Text("hi".to_string().into()))
            .unwrap();
        crate::vdbe::result::make_record(&mut vm, &Instruction::new(Opcode::MakeRecord, 3, 2, 5))
            .unwrap();
        open_pseudo(&mut vm, &Instruction::new(Opcode::OpenPseudo, 0, 5, 0)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 10)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 11)).unwrap();
        assert_eq!(*vm.register(10).unwrap(), Value::Integer(7));
        assert_eq!(
            *vm.register(11).unwrap(),
            Value::Text("hi".to_string().into())
        );
    }

    #[test]
    fn column_on_pseudo_cursor_with_non_blob_register_errors() {
        let mut vm = Vm::new();
        vm.set_register(5, Value::Integer(1)).unwrap();
        open_pseudo(&mut vm, &Instruction::new(Opcode::OpenPseudo, 0, 5, 0)).unwrap();
        let err = column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 10)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn repeated_column_reads_on_the_same_row_reuse_the_header_cache() {
        // Reads both columns of the same row twice each, in a scrambled
        // order — the second read of a column must return the same value
        // as the first (proving the cached header, populated on the very
        // first `Column` call for this row, is being reused rather than
        // silently ignored or corrupted across repeated lookups).
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 999, 0)).unwrap();

        column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 10)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 11)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 12)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 13)).unwrap();

        assert_eq!(*vm.register(11).unwrap(), Value::Integer(1));
        assert_eq!(*vm.register(13).unwrap(), Value::Integer(1));
        assert_eq!(
            *vm.register(10).unwrap(),
            Value::Text("row number 1".to_string().into())
        );
        assert_eq!(vm.register(10).unwrap(), vm.register(12).unwrap());
    }

    #[test]
    fn next_invalidates_the_header_cache_so_column_reads_the_new_row() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 999, 0)).unwrap();

        // Populate row 1's header cache, then advance — if `Next` failed
        // to invalidate it, this read would wrongly reuse row 1's offsets
        // against row 2's payload.
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 10)).unwrap();
        next(&mut vm, &Instruction::new(Opcode::Next, 0, 1, 0)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 11)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 12)).unwrap();

        assert_eq!(*vm.register(10).unwrap(), Value::Integer(1));
        assert_eq!(*vm.register(11).unwrap(), Value::Integer(2));
        assert_eq!(
            *vm.register(12).unwrap(),
            Value::Text("row number 2".to_string().into())
        );
    }

    #[test]
    fn seek_rowid_invalidates_the_header_cache() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 999, 0)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 10)).unwrap();

        vm.set_register(5, Value::Integer(1500)).unwrap();
        seek_rowid(&mut vm, &Instruction::new(Opcode::SeekRowid, 0, 42, 5)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 1, 11)).unwrap();

        assert_eq!(
            *vm.register(11).unwrap(),
            Value::Text("row number 1500".to_string().into())
        );
    }

    #[test]
    fn column_on_a_cursor_with_no_current_row_errors() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        let err = column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 10)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn column_on_ephemeral_cursor_is_a_type_mismatch() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 0, 0)).unwrap();
        let err = column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 10)).unwrap_err();
        assert!(matches!(err, ExecError::CursorTypeMismatch { .. }));
    }

    #[test]
    fn rowid_on_a_cursor_with_no_current_row_errors() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        let err = rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 10, 0)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn seek_rowid_rejects_a_non_integer_target_register() {
        let mut vm = open_vm("table_multipage.db");
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 0, 2, 0)).unwrap();
        vm.set_register(5, Value::Text("nope".to_string().into()))
            .unwrap();
        let err = seek_rowid(&mut vm, &Instruction::new(Opcode::SeekRowid, 0, 42, 5)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn insert_rejects_a_non_integer_rowid_register() {
        let mut vm = writable_vm(0x0d);
        open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 1, 0)).unwrap();
        vm.set_register(0, Value::Text("nope".to_string().into()))
            .unwrap();
        vm.set_register(3, Value::Blob(vec![].into())).unwrap();
        let err = insert(&mut vm, &Instruction::new(Opcode::Insert, 0, 0, 3)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn insert_rejects_a_non_blob_record_register() {
        let mut vm = writable_vm(0x0d);
        open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 1, 0)).unwrap();
        vm.set_register(0, Value::Integer(1)).unwrap();
        vm.set_register(3, Value::Integer(2)).unwrap();
        let err = insert(&mut vm, &Instruction::new(Opcode::Insert, 0, 0, 3)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn delete_on_a_table_cursor_with_no_current_row_errors() {
        let mut vm = writable_vm(0x0d);
        open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 1, 0)).unwrap();
        let err = delete(&mut vm, &Instruction::new(Opcode::Delete, 0, 0, 0)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn delete_on_a_pseudo_cursor_is_a_type_mismatch() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(1)).unwrap();
        open_pseudo(&mut vm, &Instruction::new(Opcode::OpenPseudo, 0, 0, 0)).unwrap();
        let err = delete(&mut vm, &Instruction::new(Opcode::Delete, 0, 0, 0)).unwrap_err();
        assert!(matches!(err, ExecError::CursorTypeMismatch { .. }));
    }

    #[test]
    fn idx_insert_on_a_pseudo_cursor_is_a_type_mismatch() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(1)).unwrap();
        open_pseudo(&mut vm, &Instruction::new(Opcode::OpenPseudo, 0, 0, 0)).unwrap();
        let err = idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)),
        )
        .unwrap_err();
        assert!(matches!(err, ExecError::CursorTypeMismatch { .. }));
    }

    #[test]
    fn idx_delete_on_a_pseudo_cursor_is_a_type_mismatch() {
        let mut vm = Vm::new();
        vm.set_register(0, Value::Integer(1)).unwrap();
        open_pseudo(&mut vm, &Instruction::new(Opcode::OpenPseudo, 0, 0, 0)).unwrap();
        let err = idx_delete(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxDelete, 0, 0, 0, P4::Int(1)),
        )
        .unwrap_err();
        assert!(matches!(err, ExecError::CursorTypeMismatch { .. }));
    }

    #[test]
    fn idx_le_holds_vacuously_before_any_probe_and_tracks_the_last_key() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 0, 0)).unwrap();
        vm.set_register(0, Value::Integer(5)).unwrap();
        // No probe/insert yet: `last_key` is None, so IdxLE holds
        // vacuously and jumps.
        let step = idx_le(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxLE, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(step, Step::Jump(99));

        idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)),
        )
        .unwrap();

        // Probe with a larger key: last_key (5) <= probe (10) holds.
        vm.set_register(0, Value::Integer(10)).unwrap();
        let step = idx_le(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxLE, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(step, Step::Jump(99));

        // Probe with a smaller key: last_key (5) <= probe (2) does not hold.
        vm.set_register(0, Value::Integer(2)).unwrap();
        let step = idx_le(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxLE, 0, 99, 0, P4::Int(1)),
        )
        .unwrap();
        assert_eq!(step, Step::Next);
    }

    #[test]
    fn new_rowid_autoincrement_rejects_a_non_str_p4() {
        let mut vm = writable_vm(0x0d);
        open_write(&mut vm, &Instruction::new(Opcode::OpenWrite, 0, 1, 0)).unwrap();
        let mut instr = Instruction::new(Opcode::NewRowid, 0, 5, 0);
        instr.p5 = 1;
        let err = new_rowid(&mut vm, &instr).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn create_table_then_drop_table_round_trip_through_sqlite_master() {
        let mut vm = writable_vm(0x0d);
        create_table(
            &mut vm,
            &Instruction::with_p4(
                Opcode::CreateTable,
                0,
                0,
                0,
                P4::CreateTable {
                    name: "t".to_string(),
                    sql: "CREATE TABLE t (a)".to_string(),
                },
            ),
        )
        .unwrap();

        // The new table's root page is now registered in sqlite_master
        // (page 1) -- read it back to prove CreateTable actually wrote it.
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 1, 1, 0)).unwrap();
        rewind(&mut vm, &Instruction::new(Opcode::Rewind, 1, 999, 0)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 1, 0, 20)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 1, 1, 21)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 1, 3, 22)).unwrap();
        assert_eq!(
            *vm.register(20).unwrap(),
            Value::Text("table".to_string().into())
        );
        assert_eq!(
            *vm.register(21).unwrap(),
            Value::Text("t".to_string().into())
        );
        let root_page = match vm.register(22).unwrap() {
            Value::Integer(n) => u32::try_from(*n).unwrap(),
            other => panic!("expected integer rootpage, got {other:?}"),
        };

        drop_table(
            &mut vm,
            &Instruction::with_p4(
                Opcode::DropTable,
                0,
                0,
                0,
                P4::DropTable {
                    name: "t".to_string(),
                    root_page,
                    indexes: vec![],
                },
            ),
        )
        .unwrap();

        // sqlite_master is now empty again.
        let step = rewind(&mut vm, &Instruction::new(Opcode::Rewind, 1, 999, 0)).unwrap();
        assert_eq!(step, Step::Jump(999));
    }

    #[test]
    fn create_index_then_drop_index_round_trip_through_sqlite_master() {
        let mut vm = writable_vm(0x0d);
        create_table(
            &mut vm,
            &Instruction::with_p4(
                Opcode::CreateTable,
                0,
                0,
                0,
                P4::CreateTable {
                    name: "t".to_string(),
                    sql: "CREATE TABLE t (a)".to_string(),
                },
            ),
        )
        .unwrap();
        open_read(&mut vm, &Instruction::new(Opcode::OpenRead, 1, 1, 0)).unwrap();
        rewind(&mut vm, &Instruction::new(Opcode::Rewind, 1, 999, 0)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 1, 3, 22)).unwrap();
        let table_root = match vm.register(22).unwrap() {
            Value::Integer(n) => u32::try_from(*n).unwrap(),
            other => panic!("expected integer rootpage, got {other:?}"),
        };

        create_index(
            &mut vm,
            &Instruction::with_p4(
                Opcode::CreateIndex,
                0,
                0,
                0,
                P4::CreateIndex {
                    name: "idx".to_string(),
                    table_name: "t".to_string(),
                    table_root_page: table_root,
                    sql: "CREATE INDEX idx ON t (a)".to_string(),
                    column_indices: vec![0],
                    unique: false,
                },
            ),
        )
        .unwrap();

        // sqlite_master now has two rows: the table and the index.
        let mut names = Vec::new();
        let step = rewind(&mut vm, &Instruction::new(Opcode::Rewind, 1, 999, 0)).unwrap();
        assert_eq!(step, Step::Next);
        let mut index_root_for_drop = None;
        loop {
            column(&mut vm, &Instruction::new(Opcode::Column, 1, 0, 30)).unwrap();
            column(&mut vm, &Instruction::new(Opcode::Column, 1, 1, 31)).unwrap();
            column(&mut vm, &Instruction::new(Opcode::Column, 1, 3, 32)).unwrap();
            names.push((
                vm.register(30).unwrap().clone(),
                vm.register(31).unwrap().clone(),
            ));
            if vm.register(30).unwrap() == &Value::Text("index".to_string().into()) {
                if let Value::Integer(n) = vm.register(32).unwrap() {
                    index_root_for_drop = Some(u32::try_from(*n).unwrap());
                }
            }
            match next(&mut vm, &Instruction::new(Opcode::Next, 1, 1, 0)).unwrap() {
                Step::Jump(1) => continue,
                Step::Next => break,
                other => panic!("unexpected step {other:?}"),
            }
        }
        assert_eq!(
            names,
            vec![
                (
                    Value::Text("table".to_string().into()),
                    Value::Text("t".to_string().into())
                ),
                (
                    Value::Text("index".to_string().into()),
                    Value::Text("idx".to_string().into())
                ),
            ]
        );

        drop_index(
            &mut vm,
            &Instruction::with_p4(
                Opcode::DropIndex,
                0,
                0,
                0,
                P4::DropIndex {
                    name: "idx".to_string(),
                    root_page: index_root_for_drop.unwrap(),
                },
            ),
        )
        .unwrap();

        // After DropIndex, only the table row remains.
        let mut remaining = Vec::new();
        let step = rewind(&mut vm, &Instruction::new(Opcode::Rewind, 1, 999, 0)).unwrap();
        assert_eq!(step, Step::Next);
        loop {
            column(&mut vm, &Instruction::new(Opcode::Column, 1, 1, 40)).unwrap();
            remaining.push(vm.register(40).unwrap().clone());
            match next(&mut vm, &Instruction::new(Opcode::Next, 1, 1, 0)).unwrap() {
                Step::Jump(1) => continue,
                Step::Next => break,
                other => panic!("unexpected step {other:?}"),
            }
        }
        assert_eq!(remaining, vec![Value::Text("t".to_string().into())]);
    }

    #[test]
    fn create_table_rejects_a_mismatched_p4() {
        let mut vm = writable_vm(0x0d);
        let err =
            create_table(&mut vm, &Instruction::new(Opcode::CreateTable, 0, 0, 0)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn drop_table_rejects_a_mismatched_p4() {
        let mut vm = writable_vm(0x0d);
        let err = drop_table(&mut vm, &Instruction::new(Opcode::DropTable, 0, 0, 0)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn create_index_rejects_a_mismatched_p4() {
        let mut vm = writable_vm(0x0d);
        let err =
            create_index(&mut vm, &Instruction::new(Opcode::CreateIndex, 0, 0, 0)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    #[test]
    fn drop_index_rejects_a_mismatched_p4() {
        let mut vm = writable_vm(0x0d);
        let err = drop_index(&mut vm, &Instruction::new(Opcode::DropIndex, 0, 0, 0)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }

    fn open_ephemeral_table(vm: &mut Vm, cursor: i32) {
        open_ephemeral(
            vm,
            &Instruction {
                opcode: Opcode::OpenEphemeral,
                p1: cursor,
                p2: 0,
                p3: 0,
                p4: P4::None,
                p5: 1,
            },
        )
        .unwrap();
    }

    fn insert_ephemeral_row(vm: &mut Vm, cursor: i32, rowid: i64, values: &[Value]) {
        for (i, v) in values.iter().enumerate() {
            vm.set_register(20i32.saturating_add(i as i32), v.clone())
                .unwrap();
        }
        crate::vdbe::result::make_record(
            vm,
            &Instruction::new(Opcode::MakeRecord, 20, values.len() as i32, 30),
        )
        .unwrap();
        vm.set_register(31, Value::Integer(rowid)).unwrap();
        insert(vm, &Instruction::new(Opcode::Insert, cursor, 31, 30)).unwrap();
    }

    #[test]
    fn ephemeral_table_insert_errors_once_row_limit_exceeded() {
        let mut vm = Vm::new();
        open_ephemeral_table(&mut vm, 0);
        for i in 0..MAX_EPHEMERAL_ROWS as i64 {
            insert_ephemeral_row(&mut vm, 0, i + 1, &[Value::Integer(i)]);
        }
        for (i, v) in [Value::Integer(999)].iter().enumerate() {
            vm.set_register(20i32.saturating_add(i as i32), v.clone())
                .unwrap();
        }
        crate::vdbe::result::make_record(&mut vm, &Instruction::new(Opcode::MakeRecord, 20, 1, 30))
            .unwrap();
        vm.set_register(31, Value::Integer(MAX_EPHEMERAL_ROWS as i64 + 1))
            .unwrap();
        let err = insert(&mut vm, &Instruction::new(Opcode::Insert, 0, 31, 30)).unwrap_err();
        assert!(matches!(
            err,
            ExecError::EphemeralRowLimitExceeded {
                opcode: "Insert",
                limit
            } if limit == MAX_EPHEMERAL_ROWS
        ));
    }

    #[test]
    fn ephemeral_index_insert_errors_once_row_limit_exceeded() {
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 0, 0)).unwrap();
        for i in 0..MAX_EPHEMERAL_ROWS as i64 {
            vm.set_register(20, Value::Integer(i)).unwrap();
            idx_insert(
                &mut vm,
                &Instruction::with_p4(Opcode::IdxInsert, 0, 20, 0, P4::Int(1)),
            )
            .unwrap();
        }
        vm.set_register(20, Value::Integer(MAX_EPHEMERAL_ROWS as i64))
            .unwrap();
        let err = idx_insert(
            &mut vm,
            &Instruction::with_p4(Opcode::IdxInsert, 0, 20, 0, P4::Int(1)),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ExecError::EphemeralRowLimitExceeded {
                opcode: "IdxInsert",
                limit
            } if limit == MAX_EPHEMERAL_ROWS
        ));
    }

    #[test]
    fn ephemeral_table_rewind_on_empty_cursor_jumps_to_p2() {
        let mut vm = Vm::new();
        open_ephemeral_table(&mut vm, 0);
        let step = rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 99, 0)).unwrap();
        assert_eq!(step, Step::Jump(99));
    }

    #[test]
    fn ephemeral_table_insert_then_full_scan_reads_rows_in_order() {
        let mut vm = Vm::new();
        open_ephemeral_table(&mut vm, 0);
        insert_ephemeral_row(&mut vm, 0, 1, &[Value::Integer(10)]);
        insert_ephemeral_row(&mut vm, 0, 2, &[Value::Integer(20)]);
        insert_ephemeral_row(&mut vm, 0, 3, &[Value::Integer(30)]);

        let step = rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 99, 0)).unwrap();
        assert_eq!(step, Step::Next);

        let mut seen = Vec::new();
        loop {
            rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 10, 0)).unwrap();
            column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 11)).unwrap();
            seen.push((
                vm.register(10).unwrap().clone(),
                vm.register(11).unwrap().clone(),
            ));
            match next(&mut vm, &Instruction::new(Opcode::Next, 0, 1, 0)).unwrap() {
                Step::Jump(1) => continue,
                Step::Next => break,
                other => panic!("unexpected step {other:?}"),
            }
        }

        assert_eq!(
            seen,
            vec![
                (Value::Integer(1), Value::Integer(10)),
                (Value::Integer(2), Value::Integer(20)),
                (Value::Integer(3), Value::Integer(30)),
            ]
        );
    }

    /// A `with_db` `Vm` whose header reports `encoding` — the source is
    /// never actually read by an `EphemeralTable` insert/scan (#266), so
    /// `minimal_writable_db`'s backing memory-VFS db just needs to parse.
    fn ephemeral_vm_with_encoding(encoding: TextEncoding) -> Vm {
        let (vfs, mut header) = minimal_writable_db(512, 0x0d);
        header.text_encoding = encoding;
        let source = crate::vfs::VfsPageSource::open(&vfs, Path::new("/test.db"), 512).unwrap();
        Vm::with_db(Rc::new(source), header)
    }

    #[test]
    fn ephemeral_table_insert_decodes_using_database_text_encoding() {
        let mut vm = ephemeral_vm_with_encoding(TextEncoding::Utf16Le);
        open_ephemeral_table(&mut vm, 0);

        // Built directly with `encode_record`/`TextEncoding::Utf16Le`,
        // bypassing `MakeRecord` (which still hardcodes UTF-8 encoding,
        // a separate, wider-scope gap tracked outside #266).
        let payload = encode_record(&[Value::Text("héllo".into())], TextEncoding::Utf16Le);
        vm.set_register(30, Value::Blob(payload.into())).unwrap();
        vm.set_register(31, Value::Integer(1)).unwrap();
        insert(&mut vm, &Instruction::new(Opcode::Insert, 0, 31, 30)).unwrap();

        rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 99, 0)).unwrap();
        column(&mut vm, &Instruction::new(Opcode::Column, 0, 0, 11)).unwrap();
        assert_eq!(*vm.register(11).unwrap(), Value::Text("héllo".into()));
    }

    #[test]
    fn ephemeral_table_last_positions_on_the_final_row() {
        let mut vm = Vm::new();
        open_ephemeral_table(&mut vm, 0);
        insert_ephemeral_row(&mut vm, 0, 1, &[Value::Integer(10)]);
        insert_ephemeral_row(&mut vm, 0, 2, &[Value::Integer(20)]);

        let step = last(&mut vm, &Instruction::new(Opcode::Last, 0, 99, 0)).unwrap();
        assert_eq!(step, Step::Next);
        rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 10, 0)).unwrap();
        assert_eq!(*vm.register(10).unwrap(), Value::Integer(2));
    }

    #[test]
    fn ephemeral_table_index_mode_default_still_rejects_rewind() {
        // P5 zero (the default) must keep opening the existing index-mode
        // ephemeral cursor — DISTINCT's dedup path must not regress.
        let mut vm = Vm::new();
        open_ephemeral(&mut vm, &Instruction::new(Opcode::OpenEphemeral, 0, 0, 0)).unwrap();
        let err = rewind(&mut vm, &Instruction::new(Opcode::Rewind, 0, 99, 0)).unwrap_err();
        assert!(matches!(err, ExecError::CursorTypeMismatch { .. }));
    }

    #[test]
    fn ephemeral_table_rowid_with_no_current_row_errors() {
        let mut vm = Vm::new();
        open_ephemeral_table(&mut vm, 0);
        let err = rowid(&mut vm, &Instruction::new(Opcode::Rowid, 0, 10, 0)).unwrap_err();
        assert!(matches!(err, ExecError::MalformedInstruction { .. }));
    }
}
