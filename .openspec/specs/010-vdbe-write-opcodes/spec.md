---
domain: vdbe
version: 0.1.0
status: draft
date: 2026-08-19
---

# 010 — VDBE Write Opcodes

The VDBE's write path: `MakeRecord`'s type-affinity coercion, `Insert`,
`Delete` (real b-tree path), `IdxInsert` (real b-tree path), and
`NewRowid`, on top of the b-tree write functions merged by #168/#169.
Part of V3 phase 3 (epic #161), tracked by #194.

## Philosophy

Spec 009's philosophy holds here unchanged: the VDBE is a dumb dispatcher,
never re-deriving semantics the kernel (spec 008) or the b-tree layer
(spec 006) already define. This spec adds no new record-encoding or
b-tree-mutation logic of its own — `MakeRecord` reuses spec 003's
`encode_record` byte-for-byte (as it already did for the read/DISTINCT
path), and `Insert`/`Delete`/`IdxInsert` are thin operand-marshalling
wrappers over `src/btree::{insert_row, delete_row, insert_entry}`.

Unlike spec 009's opcodes, `OpenWrite`/`Insert`/`NewRowid` were never
harvested from a V2-era oracle `EXPLAIN` (V2 predates any write-path
support), so `tools/opcodes-v2.json`/`Opcode::ALL`'s harvested-parity
check (`tests/vdbe/opcode_completeness_test.rs`) deliberately does not
cover them — see `src/vdbe/program.rs`'s `ALL` doc comment. This spec's
requirements are the traceability surface for those three opcodes
instead.

## Requirements

### Requirement 1: Write-capable `Vm` [MUST]

A `Vm` MUST be constructible in a write-capable mode
(`Vm::with_writable_db`/`execute_with_writable_db`) that can service both
ordinary read cursors (`OpenRead`) and the write opcodes below against one
shared underlying `Pager`, without duplicating page state between the two
access paths. A `Vm` built via the pre-existing read-only
`Vm::with_db`/`execute_with_db` MUST reject every write opcode with
`ExecError::NoDatabase` rather than silently no-op or panic.

**Implementation:** `src/vdbe/exec.rs` (`VmDb`, `Vm::with_writable_db`,
`execute_with_writable_db`); `src/pager.rs` (`impl PageSource for
RefCell<Pager>`)

#### Scenario: OpenWrite against a read-only Vm errors instead of silently opening

- GIVEN a `Vm` built via `Vm::with_db` (read-only)
- WHEN `OpenWrite` runs
- THEN it returns `Err(ExecError::NoDatabase { .. })`

**Tests:** `src/vdbe/cursor.rs::tests::open_write_requires_a_writable_vm`

#### Scenario: A write program's committed changes are visible to an independent reader

- GIVEN a real temp-file database opened via `UnixVfs`/`Pager::open`
- WHEN a `Program` opens a write cursor, computes a new rowid, encodes a
  record, and inserts it, then halts successfully
- THEN a second, independent `Pager`/`TableCursor` opened on the same file
  path after the writing `Vm` has gone out of scope reads back the exact
  row inserted

**Tests:**
`tests/vdbe/write_opcodes_test.rs::insert_round_trips_through_v1_reader_on_a_real_temp_file`

### Requirement 2: MakeRecord affinity [MUST]

`MakeRecord` MUST, when its `P4` operand is a `P4::Affinity` byte string,
apply the affinity byte at each position (via
`src/vdbe/affinity.rs::apply_affinity`) to a *copy* of the corresponding
source register — in register order, one byte per column — before
encoding the row via `encode_record`. A byte string shorter than the
register range MUST leave the remaining trailing columns un-coerced. Any
other `P4` (including absent) MUST leave `MakeRecord` exactly as
pre-#194 (no affinity coercion at all), so every existing caller of this
opcode (the DISTINCT/`ResultRow` read path) is unaffected.

**Implementation:** `src/vdbe/result.rs::make_record`

#### Scenario: A numeric-looking TEXT register is coerced to INTEGER before encoding

- GIVEN registers holding `Text("42")` and `Text("abc")`
- WHEN `MakeRecord` runs with `P4::Affinity(['D', 'B'])` (INTEGER, TEXT)
- THEN the encoded record decodes as `[Integer(42), Text("abc")]`, and the
  source registers are unchanged (affinity applies to a copy)

**Tests:**
`src/vdbe/result.rs::tests::make_record_applies_p4_affinity_before_encoding`

#### Scenario: Absent P4 leaves MakeRecord's pre-#194 behavior unchanged

- GIVEN a register holding `Text("42")`
- WHEN `MakeRecord` runs with `P4::None`
- THEN the encoded record decodes as `[Text("42")]` — no affinity
  coercion

**Tests:**
`src/vdbe/result.rs::tests::make_record_without_affinity_p4_is_unchanged_from_pre_194_behavior`,
`src/vdbe/result.rs::tests::make_record_output_matches_spec_003_encoding`

### Requirement 3: Insert [MUST]

`Insert` MUST insert a row into the table b-tree cursor `P1` is open on
(a real write cursor opened by `OpenWrite`), reading the rowid from the
integer register `P2` and the already-`MakeRecord`-encoded payload blob
from register `P3`, delegating to `btree::insert_row`. `OR
REPLACE`/`OR IGNORE`-style conflict resolution is out of scope — every
insert is unconditional, matching `insert_row`'s own contract (a
duplicate rowid surfaces as `ExecError::MalformedInstruction` wrapping
`BtreeError::DuplicateRowid`).

**Implementation:** `src/vdbe/cursor.rs::insert`

#### Scenario: Insert writes a row readable by V1's own TableCursor/decode_record

- GIVEN an open write cursor on an empty table b-tree, rowid register 1,
  and a record register holding `MakeRecord`'s output for `(42, "hello")`
- WHEN `Insert` runs
- THEN a fresh `TableCursor` positioned via `first()` returns rowid 1 and
  a payload that decodes to `[Integer(42), Text("hello")]`

**Tests:**
`src/vdbe/cursor.rs::tests::insert_then_read_back_round_trips_through_make_record_and_column`,
`tests/vdbe/write_opcodes_test.rs::insert_round_trips_through_v1_reader_on_a_real_temp_file`

### Requirement 4: Delete (real cursor path) [MUST]

`Delete` MUST dispatch on the cursor slot's kind: unchanged
ephemeral-cursor behavior (spec 009 Requirement 4's DISTINCT dedup path)
for an `Ephemeral` slot; for a real table write cursor, delete the row at
the cursor's *current* position — whatever `Rewind`/`Next`/`SeekRowid`
most recently positioned it on — via `btree::delete_row`, then clear the
cursor's current-row state (a stray follow-up `Rowid`/`Column` on the
same slot must read as "no row", not stale data). `Delete` on a real
cursor with no current row (never positioned, or already exhausted) MUST
error rather than delete an arbitrary row.

**Implementation:** `src/vdbe/cursor.rs::delete`

#### Scenario: Delete removes the row a real cursor is positioned on

- GIVEN a table b-tree holding one row, and a write cursor `Rewind`
  positioned it on
- WHEN `Delete` runs
- THEN the row is gone from the on-disk b-tree — a following `Rewind`
  reports the table empty

**Tests:**
`src/vdbe/cursor.rs::tests::delete_removes_the_row_at_the_cursors_current_position`,
`tests/vdbe/write_opcodes_test.rs::delete_removes_a_previously_inserted_row_from_the_on_disk_file`

#### Scenario: Delete on an ephemeral cursor is unaffected by the real-cursor path

- GIVEN an ephemeral (DISTINCT) cursor with a probed/inserted key
- WHEN `Delete` runs
- THEN the ephemeral entry is removed exactly as before #194

**Tests:** `src/vdbe/cursor.rs::tests::delete_removes_the_just_probed_duplicate_row`

### Requirement 5: IdxInsert (real cursor path) [MUST]

`IdxInsert` MUST dispatch on the cursor slot's kind: unchanged
ephemeral-cursor behavior (spec 009 Requirement 4) for an `Ephemeral`
slot; for a real `CursorSlot::IndexWrite` cursor (opened by `OpenWrite`
with `P5` nonzero), encode the register range `[P2, P2+count)` (`count`
from `P4::Int`) as a full index entry and insert it into the on-disk
index (or WITHOUT ROWID table) b-tree via `btree::insert_entry`. A
duplicate key surfaces as `ExecError::MalformedInstruction` wrapping
`BtreeError::DuplicateKey` — `OR IGNORE`/`OR REPLACE` resolution is out
of scope.

**Implementation:** `src/vdbe/cursor.rs::idx_insert`

#### Scenario: IdxInsert on a real index cursor writes an entry readable by IndexCursor

- GIVEN an open real index write cursor on an empty index b-tree, and
  registers holding `Integer(5)`, `Text("x")`
- WHEN `IdxInsert` runs with `P4::Int(2)`
- THEN a fresh `IndexCursor::first()` on the same root page returns one
  entry decoding to `[Integer(5), Text("x")]`

**Tests:**
`src/vdbe/cursor.rs::tests::idx_insert_real_cursor_writes_an_index_entry_readable_by_index_cursor`

### Requirement 6: NewRowid [MUST]

`NewRowid` MUST write `max(rowid) + 1` (or `1` for an empty table, via
`TableCursor::last()`) for table cursor `P1` into register `P2`.

**AUTOINCREMENT simplification (documented, not hidden):** this VDBE
layer has no schema-aware way to know whether a table was declared
`INTEGER PRIMARY KEY AUTOINCREMENT` — that bit lives in the schema/
codegen, neither of which this ticket's VDBE-layer scope touches. Rather
than block on schema plumbing that doesn't exist yet, AUTOINCREMENT
handling is opt-in per instruction: when `P5` is nonzero and `P4` carries
the table's name (`P4::Str`), `NewRowid` additionally consults/bumps
`sqlite_sequence` (`btree::ensure_sqlite_sequence_table`/
`update_sequence`), taking `max(TableCursor::last() rowid,
sqlite_sequence.seq) + 1` — matching stock SQLite's own "the sequence
counter never regresses, even after the row it recorded is deleted"
behavior. Without `P5`/`P4`, `NewRowid` is plain non-AUTOINCREMENT rowid
allocation. Wiring "is this table AUTOINCREMENT" from the schema into
`P5`/`P4` at codegen time is out of scope for this ticket (no codegen
change here) — a future codegen ticket sets these operands once it knows.

**Implementation:** `src/vdbe/cursor.rs::new_rowid`

#### Scenario: NewRowid on an empty table starts at 1

- GIVEN an empty table b-tree
- WHEN `NewRowid` runs (no AUTOINCREMENT flag)
- THEN register `P2` holds `Integer(1)`

**Tests:** `src/vdbe/cursor.rs::tests::new_rowid_starts_at_one_on_an_empty_table`

#### Scenario: NewRowid after an insert skips past the max existing rowid

- GIVEN a table b-tree with one row at rowid 5
- WHEN `NewRowid` runs (no AUTOINCREMENT flag)
- THEN register `P2` holds `Integer(6)`

**Tests:**
`src/vdbe/cursor.rs::tests::new_rowid_after_insert_skips_past_the_max_existing_rowid`

#### Scenario: AUTOINCREMENT-flagged NewRowid consults and bumps sqlite_sequence

- GIVEN an empty table b-tree and no prior `sqlite_sequence` entry for
  table `"t"`
- WHEN `NewRowid` runs twice with `P5` nonzero and `P4::Str("t")`, with no
  actual row inserted between calls
- THEN the first call returns `1` and the second returns `2` — `
  sqlite_sequence`'s tracked value strictly increases across calls rather
  than recomputing the same `TableCursor::last()`-derived value

**Tests:**
`src/vdbe/cursor.rs::tests::new_rowid_autoincrement_consults_and_bumps_sqlite_sequence`

## Related regimes

- Tier suite: `tests/tiers/tier2.rs`'s `t2_crud_round_trips_on_rowid_tables`
  stub covers full CRUD (including `UPDATE`, not part of this ticket's
  opcode set) — left `#[ignore]`; a future ticket adding `UPDATE`
  support (likely compiled as `Delete`+`Insert` or a dedicated opcode)
  should flip it once the full round trip is codegen-reachable, not just
  hand-assembled as this spec's tests do.
- Parity suite (#72, VM-diff against oracle `EXPLAIN`): none of this
  spec's three new opcodes (`OpenWrite`/`Insert`/`NewRowid`) were
  harvested from a V2-era oracle, so they carry no parity-suite
  dimension yet — see the Philosophy section above.
- Corpus follow-on: none opened by this ticket; `Insert`/`Delete`/
  `IdxInsert` reuse #168/#169's already-corpus-tested b-tree write
  functions without adding new b-tree-level behavior.
