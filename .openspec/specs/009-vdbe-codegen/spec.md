---
domain: vdbe
version: 0.1.0
status: draft
date: 2026-08-16
---

# 009 — VDBE Codegen

The bytecode virtual machine — SQLite's `vdbe.c`/`vdbeaux.c` instruction set
and register model, plus the codegen that emits it from the V2 AST (#61).
Backs V2 phase 3 (#89/#90/#91), part of epic #56. The opcode set is frozen
by the phase-3 opener (#87): the harvested, scope-decided
`tools/opcodes-v2.json` (52 opcodes, oracle 3.53.3) is the exhaustive
denominator for every requirement below — no opcode outside that inventory
is in scope for V2, and every opcode inside it must appear as a scenario
somewhere in this spec (#65 wires the count into the assurance dashboard).
Refs: 001/Req-3.

## Philosophy

SQLite has one VM, one instruction set, and one place semantics live: the
value-semantics kernel (spec 008, `src/vdbe/{compare,affinity,collation,
coerce,value}.rs`). The VDBE itself is a dumb dispatcher over that kernel —
it owns control flow (jumps, subroutines, loop counters), register storage,
and cursor plumbing, but it never re-derives a comparison, coercion, or
collation rule the kernel already defines. This mirrors the epic's adopted
architecture decision (epic #56 body): no standalone expression evaluator,
ever — expressions compile to opcodes, and opcodes call the kernel.

As with specs 004/008, this spec does not invent instruction semantics —
every opcode's behavior traces to the pinned oracle's `EXPLAIN` output
(`tools/opcodes-v2.json`, harvested by spike 007 / #58, re-harvested and
scope-frozen by #87) or, where the harvest under-samples emission shape
(expression control flow), to SQLite's own bytecode as observed via
`EXPLAIN` on the pinned oracle — not a paraphrase of generic VM design.

Grammar is untouched by this spec (see `.openspec/grammar/sqlite.ebnf`);
this is the layer immediately below codegen's input (the V2 AST, #61) and
immediately above the value-semantics kernel (spec 008).

## Requirements

### Requirement 1: Instruction Format [MUST]

Every instruction MUST be a fixed-shape record: an opcode tag, three
integer operands `P1`/`P2`/`P3` (`i32`), one dynamically-typed operand `P4`
(absent, an integer, a string, or a comparison/collation descriptor
depending on opcode), and one small operand `P5` (`u16`, opcode-specific
bit flags) — matching SQLite's own `Op` struct shape. A `Program` MUST be
a linear, zero-indexed array of instructions; execution MUST start at
program counter (PC) 0 and advance by incrementing PC unless an
instruction explicitly redirects it (jump, subroutine call/return, or
`Halt`).

**Implementation:** `src/vdbe/program.rs` (#89)

#### Scenario: An instruction carries opcode, three integer operands, dynamic P4, and P5 flags

- GIVEN the harvested `Ge` instruction from `SELECT * FROM products WHERE
  price >= 10 AND qty < 50` (`tools/opcodes-v2.json`), whose P4 variant is
  `"BINARY-8"`
- THEN the instruction's P4 slot holds a collation-sequence descriptor
  (`"BINARY-8"`: BINARY collation, affinity byte `8`/NUMERIC), not an
  integer or absent value — P4's type is opcode- and call-site-dependent,
  not fixed per opcode

**Tests:** `src/vdbe/program.rs::tests::instruction_carries_typed_p4_variant`

#### Scenario: Program execution starts at PC 0 and advances linearly absent a jump

- GIVEN a `Program` compiled from `SELECT * FROM products` (harvested
  shape: `Init` → `OpenRead` → `Rewind` → loop body → `Halt`, per
  `tools/opcodes-v2.json`'s `example_query` fields)
- WHEN execution begins
- THEN the first instruction executed is at index 0 (`Init`), and PC
  advances by exactly 1 after every instruction that is not itself a jump,
  call, or `Halt`

**Tests:** `src/vdbe/exec.rs::tests::hand_assembled_program_computes_1_plus_2_and_emits_a_row`

### Requirement 2: Register Model [MUST]

The VM MUST hold a register file of `Value` cells (spec 008's `Value`
type, `src/vdbe/value.rs`), indexed by the same `i32` operand space as
`P1`/`P2`/`P3`; a register's contents MUST persist across instructions
until explicitly overwritten (no implicit clearing between opcodes). The
VM MUST hold a separate cursor-slot table (one open cursor or ephemeral
table per slot, addressed the same way as registers but in a disjoint
namespace) and a small integer comparison-flags/last-compare-result cell
used by control-flow opcodes (`IfNot`/`IfPos`/`DecrJumpZero` — see
Requirement 3) to make their jump decision without re-reading a register.

**Implementation:** `src/vdbe/exec.rs` (#89; cursor-slot table itself is #90)

#### Scenario: A register retains its value across unrelated instructions

- GIVEN a register loaded via `Integer` (harvested from `SELECT * FROM
  products WHERE price > 10`) and a subsequent `Column` instruction that
  writes to a different register
- THEN the `Integer`-loaded register's value is unchanged after the
  `Column` instruction executes — registers are not implicitly cleared

**Tests:** `src/vdbe/exec.rs::tests::register_persists_across_unrelated_instructions`

#### Scenario: Cursor slots and registers occupy disjoint address spaces

- GIVEN `OpenRead` allocating cursor slot 0 (harvested from `SELECT * FROM
  products`) and `Integer` allocating register 0 in the same program
- THEN reading cursor slot 0 (via `Column`) and reading register 0 (via
  `ResultRow`) access independent storage — a cursor-slot index and a
  register index of the same integer value never alias

**Tests:** `src/vdbe/exec.rs::tests::cursor_slots_and_registers_are_disjoint`

### Requirement 3: Control-Flow Opcodes [MUST]

The 15 control-category opcodes in `tools/opcodes-v2.json` — `Init`,
`Goto`, `Once`, `BeginSubrtn`, `Return`, `Halt`, `Transaction`, `IfNot`,
`IfNotZero`, `IfPos`, `DecrJumpZero`, `IsNull`, `NotNull`, `MustBeInt`, and
`OffsetLimit` — MUST implement unconditional jump (`Goto`), one-shot
guarding (`Once`: jump past initialization code on every execution after
the first), subroutine call/return (`BeginSubrtn`/`Return`, via a return
address stored in a register), NULL-testing conditional jumps
(`IsNull`/`NotNull`), and the LIMIT/OFFSET counter family
(`OffsetLimit` computes a combined counter from separately-supplied LIMIT
and OFFSET registers; `IfPos`/`IfNotZero`/`DecrJumpZero` decrement and
branch on that counter per row). `Halt` MUST terminate execution and
signal success or a specific SQLite error code via its operands.

**Implementation:** `src/vdbe/control.rs` (#89)

#### Scenario: LIMIT/OFFSET decomposes into a setup opcode and per-row counter opcodes

- GIVEN `SELECT * FROM products LIMIT 2 OFFSET 1` (harvested: `OffsetLimit`
  once, `MustBeInt` once, `IfPos` once — per `tools/opcodes-v2.json`)
- THEN `OffsetLimit` runs once before the scan loop to combine LIMIT and
  OFFSET into a single row-budget counter, and `IfPos` runs once per
  iteration inside the loop to test and decrement that counter — LIMIT/
  OFFSET is control flow, not a dedicated single opcode

**Tests:** `src/vdbe/control.rs::tests::offset_limit_combines_limit_and_offset`

#### Scenario: DecrJumpZero implements the LIMIT-without-OFFSET counter form

- GIVEN `SELECT * FROM products LIMIT 2` (harvested: `DecrJumpZero` twice —
  once per matching row up to the limit, per `tools/opcodes-v2.json`)
- THEN each `DecrJumpZero` execution decrements the counter register and
  jumps out of the scan loop the instant it reaches zero, without needing
  a separate `OffsetLimit` setup when OFFSET is absent

**Tests:** `src/vdbe/control.rs::tests::decr_jump_zero_terminates_at_zero`

#### Scenario: Once guards subroutine initialization on repeat entry

- GIVEN `SELECT * FROM products WHERE id IN (1, 2, 3)` (harvested:
  `BeginSubrtn`, `Once`, `Return`, `NullRow` — per `tools/opcodes-v2.json`)
- THEN `Once` executes its guarded initialization the first time control
  reaches it and jumps past that block on every subsequent execution
  within the same statement run

**Tests:** `src/vdbe/control.rs::tests::once_falls_through_first_time_then_jumps_on_repeat_entry`

### Requirement 4: Cursor Opcodes [MUST]

The 15 cursor-category opcodes — `OpenRead`, `OpenEphemeral`,
`OpenPseudo`, `Rewind`, `Last`, `Next`, `Column`, `Rowid`, `SeekRowid`,
`NullRow`, `Sequence`, `Found`, `IdxInsert`, `IdxLE`, and `Delete` — MUST
drive V1's cursor API (`src/btree` read cursors) for real tables
(`OpenRead`/`Rewind`/`Next`/`Column`/`Rowid`/`SeekRowid`) and an in-memory
ephemeral `BTreeMap`-backed cursor (`OpenEphemeral`, per the epic's
DISTINCT scope decision, #87) for scratch storage that never touches the
on-disk file format. `OpenPseudo` MUST open a single-row pseudo-cursor
used to re-present an already-computed record (e.g. the ORDER BY sorter's
output row) as if it were a table cursor, so downstream `Column` opcodes
need no special case. `Found`/`IdxInsert`/`Delete` MUST implement
DISTINCT's dedup path: probe the ephemeral index, insert if absent, delete
the just-produced duplicate row if present.

**Implementation:** `src/vdbe/cursor.rs` (#90)

#### Scenario: A full-table scan opens, rewinds, iterates, and reads via cursor opcodes

- GIVEN `SELECT * FROM products` (harvested: `OpenRead` once, `Rewind`
  once, `Next` 25 times, `Column` 105 times, `Rowid` 18 times — per
  `tools/opcodes-v2.json`)
- THEN `OpenRead` opens a read cursor on the table's root page, `Rewind`
  positions it at the first row (or jumps past the loop if the table is
  empty), `Column` reads each requested column from the cursor's current
  row without advancing it, and `Next` advances to the following row or
  falls through to end the loop when exhausted

**Tests:** `src/vdbe/cursor.rs::tests::full_scan_opens_rewinds_iterates_reads`,
`tests/vdbe/cursor_sorter_test.rs::full_scan_program_matches_oracle_row_for_row`

#### Scenario: DISTINCT probes an ephemeral index before emitting each row

- GIVEN `SELECT DISTINCT note FROM products` (harvested: `OpenEphemeral`,
  `Sequence`, `Found`, `IdxInsert`, `Delete`, `MakeRecord` — per
  `tools/opcodes-v2.json`)
- THEN each candidate output row is probed against the ephemeral index via
  `Found`; a row not already present is inserted via `IdxInsert` and
  passed through to `ResultRow`, while a row already present is discarded
  — the ephemeral index is backed by an in-memory `BTreeMap`, never a
  page-format file structure

**Tests:** `src/vdbe/cursor.rs::tests::distinct_probes_ephemeral_index_before_emit`,
`tests/vdbe/cursor_sorter_test.rs::distinct_program_discards_rows_already_seen`

#### Scenario: Equality lookup uses SeekRowid instead of a full scan

- GIVEN `SELECT * FROM products WHERE id = 2` (harvested: `SeekRowid`
  three times — per `tools/opcodes-v2.json`)
- THEN `SeekRowid` positions the cursor directly at the row with the given
  rowid (or jumps past the row-handling code if no such row exists),
  skipping `Rewind`/`Next` scan iteration entirely

**Tests:** `src/vdbe/cursor.rs::tests::seek_rowid_skips_full_scan_on_pk_equality`

### Requirement 5: Compare Opcodes [MUST]

The 6 compare-category opcodes — `Eq`, `Ge`, `Gt`, `Le`, `Lt`, and
`RealAffinity` — MUST delegate their value comparison to spec 008's
`src/vdbe/compare.rs` (cross-type ordering, Requirement 2 there) and
collation dispatch to `src/vdbe/collation.rs` (Requirement 3 there); the
opcode layer supplies only the jump-on-result control flow and the P4
collation-sequence descriptor (e.g. `"BINARY-8"`: collating function name
plus operand affinity byte), never a second comparison rule. `RealAffinity`
is filed under `compare` (not a dedicated coercion category — none exists
in the harvested taxonomy, per spike 007's findings) because it is
affinity coercion applied on cursor-column load, upstream of any actual
comparison, and MUST delegate to spec 008's `src/vdbe/affinity.rs`
(Requirement 1 there) rather than reimplementing the coercion rule.

**Implementation:** `src/vdbe/exec.rs` (#89; comparison/affinity
delegation target is `src/vdbe/compare.rs`, existing, spec 008)

#### Scenario: Eq/Ge/Gt/Le/Lt jump on the kernel's comparison result, not a re-derived rule

- GIVEN `SELECT * FROM products WHERE price >= 10 AND qty < 50` (harvested:
  `Ge` and `Lt`, both P4 `"BINARY-8"` — per `tools/opcodes-v2.json`)
- THEN each opcode calls spec 008's cross-type comparison (NULL < numeric
  < text < blob, Requirement 2 of spec 008) via `src/vdbe/compare.rs` and
  branches on its result; the opcode itself contains no comparison logic
  of its own

**Tests:** `src/vdbe/exec.rs::tests::compare_opcodes_jump_on_kernel_result_not_a_re_derived_rule`

#### Scenario: RealAffinity applies column affinity on load, independent of any comparison

- GIVEN `SELECT * FROM products` against a table with a REAL column
  (harvested: `RealAffinity` 28 times — once per query touching a REAL
  column, per `tools/opcodes-v2.json` and spike 007's findings)
- THEN `RealAffinity` coerces the loaded column value per spec 008's
  affinity rules (Requirement 1 there) at load time, regardless of
  whether the query performs any comparison on that column

**Tests:** `src/vdbe/exec.rs::tests::real_affinity_coerces_register_on_load_independent_of_comparison`

### Requirement 6: Arithmetic Opcodes [MUST]

The 5 arithmetic-category opcodes — `Add`, `Subtract`, `Multiply`,
`Divide`, `Remainder` — MUST delegate all overflow, NULL-propagation, and
numeric-coercion behavior to spec 008's `src/vdbe/coerce.rs` (Requirement
5 there: `i64` overflow promotes to REAL, never wraps) and `src/vdbe/
value.rs` (Requirement 4 there: NULL propagates through arithmetic); the
opcode layer supplies only register addressing (read two source
registers, write one destination register).

**Implementation:** `src/vdbe/arithmetic.rs` (#89)

#### Scenario: Add/Subtract/Divide/Remainder read two registers and write one

- GIVEN `SELECT price + 1, price - 1, price / 2, qty % 2 FROM products`
  (harvested: `Add`, `Subtract`, `Divide`, `Remainder`, each once — per
  `tools/opcodes-v2.json`)
- THEN each opcode reads its two operand registers, delegates the actual
  arithmetic (including overflow/NULL handling) to the value-semantics
  kernel, and writes the result to its destination register — the opcode
  itself performs no arithmetic

**Tests:** `src/vdbe/arithmetic.rs::tests::add_reads_two_registers_writes_one`,
`src/vdbe/arithmetic.rs::tests::null_propagates_through_every_arithmetic_opcode`,
`src/vdbe/arithmetic.rs::tests::divide_by_zero_yields_null_not_a_panic`

### Requirement 7: Function Opcode [MUST]

The single function-category opcode, `Function`, MUST dispatch by a P4
function-descriptor (name + arity, e.g. `"abs(1)"`, `"like(2)"`,
`"round(2)"`) into spec 008's scalar-function registry
(`src/vdbe/functions.rs`, Requirement 6 there), reading its argument
registers (a contiguous run starting at `P2`) and writing the result to
`P3`. `Function` MUST NOT contain any function-specific logic itself —
adding a scalar function to spec 008's registry MUST be sufficient to make
it callable via this opcode, with no VDBE-layer change required.

**Implementation:** `src/vdbe/exec.rs::function` (#91; registry itself is
`src/vdbe/functions.rs`, existing, spec 008)

#### Scenario: Function dispatches by name+arity descriptor to the shared registry

- GIVEN `SELECT * FROM products WHERE name LIKE 'g%'` (harvested:
  `Function` 6 times, with P4 variants `"abs(1)"`, `"length(1)"`,
  `"like(2)"`, `"lower(1)"`, `"round(2)"`, `"upper(1)"` across the V2
  harvest set — per `tools/opcodes-v2.json`)
- THEN each `Function` instance reads its argument registers, looks up the
  named function at the given arity in spec 008's registry, and writes
  the registry's return value to its result register — `like(2)` and
  `abs(1)` are dispatched through the identical opcode logic, differing
  only in their P4 descriptor

**Tests:** `tests/codegen/expr_test.rs::like_and_glob_dispatch_through_the_function_opcode`,
`tests/codegen/expr_test.rs::single_arg_function_call_compiles`,
`tests/codegen/expr_test.rs::multi_arg_function_call_compiles_with_contiguous_registers`

### Requirement 8: Result-Row Opcodes [MUST]

The 4 result-category opcodes — `Integer`, `String8`, `MakeRecord`,
`ResultRow` — MUST implement literal loading (`Integer`: load an `i64`
constant into a register from `P1`; `String8`: load a UTF-8 string
constant from `P4` into a register), record serialization (`MakeRecord`:
pack a contiguous run of registers into spec 003's record format, using
`P4`'s per-column serial-type hints where present), and row emission
(`ResultRow`: yield a contiguous run of registers as one output row to the
statement's caller). `MakeRecord`'s output format MUST be byte-identical
to spec 003's record encoding — this is the same on-disk record format,
reused for in-memory ephemeral rows (DISTINCT, sorter), not a
VDBE-private serialization.

**Implementation:** `src/vdbe/result.rs` (#89)

#### Scenario: Integer and String8 load typed literals into registers

- GIVEN `SELECT * FROM products WHERE price > 10` (harvested: `Integer`
  20 times) and `SELECT * FROM products WHERE name LIKE 'g%'` (harvested:
  `String8` with P4 variants `"cheap"`, `"expensive"`, `"g%"`, `"none"` —
  per `tools/opcodes-v2.json`)
- THEN `Integer` writes its `P1` operand as an `i64` into its destination
  register, and `String8` writes its `P4` string constant into its
  destination register — both are pure literal loads, no computation

**Tests:** `src/vdbe/result.rs::tests::integer_and_string8_load_literals`

#### Scenario: ResultRow emits a fixed register range as one output row every iteration

- GIVEN `SELECT * FROM products` (harvested: `ResultRow` 26 times — once
  per statement run across the harvest's 26-query set, one row-emit call
  site per query regardless of row count, per `tools/opcodes-v2.json`)
- THEN `ResultRow` yields the registers in its `[P1, P1+P2)` range as one
  logical output row to the statement's caller, without itself advancing
  any cursor or looping — looping is the scan opcodes' job (Requirement 4)

**Tests:** `src/vdbe/result.rs::tests::result_row_emits_fixed_register_range`

#### Scenario: MakeRecord's output is byte-identical to spec 003's record format

- GIVEN `SELECT DISTINCT note FROM products` (harvested: `MakeRecord` 7
  times, P4 `"D"` — per `tools/opcodes-v2.json`)
- THEN the packed record's varint header and serial-type-tagged payload
  match spec 003's `**Implementation:** src/record.rs` encoding exactly —
  `MakeRecord` calls that encoder rather than reimplementing it

**Tests:** `src/vdbe/result.rs::tests::make_record_output_matches_spec_003_encoding`

### Requirement 9: Sorter Opcodes [MUST]

The 6 sorter-category opcodes — `SorterOpen`, `SorterInsert`,
`SorterSort`, `SorterNext`, `SorterData`, and `Sort` — MUST implement
ORDER BY as a distinct opcode family from cursor scanning, not a flag on
an existing opcode: `SorterOpen` allocates an in-memory sorter keyed by
the sort-key descriptor in `P4` (e.g. `"k(2,-B,B)"`: 2 sort keys, second
descending); `SorterInsert` buffers one candidate row; `Sort` (or
`SorterSort`, depending on whether an index can pre-satisfy the order —
both appear in the harvest) triggers the actual sort; `SorterNext`/
`SorterData` then iterate the sorted result exactly as `Next`/`Column`
iterate a table cursor, feeding `OpenPseudo`'s single-row cursor
(Requirement 4) so downstream opcodes need no special case for
sorter-sourced rows.

**Implementation:** `src/vdbe/sorter.rs` (#90)

#### Scenario: ORDER BY buffers all rows into a sorter before emitting any

- GIVEN `SELECT * FROM products ORDER BY price` (harvested: `SorterOpen`,
  `SorterInsert`, `SorterSort`, `SorterNext`, `SorterData` — per
  `tools/opcodes-v2.json`)
- THEN every candidate row is inserted into the sorter via `SorterInsert`
  during an initial scan pass, `SorterSort` runs once after that pass
  completes, and only then does `SorterNext`/`SorterData` begin producing
  rows in sorted order — no row is emitted before the full input is
  buffered

**Tests:** `src/vdbe/sorter.rs::tests::order_by_buffers_all_rows_before_sorting`,
`tests/vdbe/cursor_sorter_test.rs::order_by_program_emits_rows_in_sorted_order`

#### Scenario: Sort key descriptor encodes column count and per-column direction

- GIVEN `SELECT * FROM products ORDER BY price LIMIT 1` (harvested:
  `SorterOpen` with P4 `"k(1,B)"` — 1 sort key, ascending BINARY) and a
  multi-column ORDER BY harvested elsewhere as `"k(2,-B,B)"` — 2 keys,
  first descending
- THEN the sorter's comparison during `SorterSort` follows the P4
  descriptor's column count and per-column direction/collation exactly,
  delegating the actual value comparison to spec 008 (Requirement 5 of
  this spec)

**Tests:** `src/vdbe/sorter.rs::tests::sort_key_descriptor_drives_multi_column_order`

### Requirement 10: EXPLAIN Output Format [MUST]

`EXPLAIN <stmt>` MUST render the compiled program as a table with columns
`addr` (the instruction's index), `opcode` (its tag name), `p1`, `p2`,
`p3`, `p4`, `p5` (its operands, `p4` rendered as its display form — e.g. a
string constant unquoted, a collation descriptor as its raw string), and
`comment` (a short human-readable annotation of the instruction's effect,
matching SQLite's own `EXPLAIN` comment conventions where one exists).
This format MUST be stable enough to feed parity #72's planned VM-diff
dimension: two programs for the same query, from our engine and the
pinned oracle, compared instruction-by-instruction.

**Implementation:** `src/vdbe/explain.rs` (#91)

#### Scenario: EXPLAIN renders one row per instruction with all seven columns

- GIVEN `EXPLAIN SELECT * FROM products WHERE price > 10`
- THEN the output has one row per instruction in program order, each row
  populated in all seven columns (`addr` matching the instruction's linear
  position, `p4` empty/blank when absent rather than a placeholder value)

**Tests:** `tests/vdbe/explain_test.rs::explain_renders_one_row_per_instruction_all_columns`

#### Scenario: EXPLAIN's p4 column renders the operand's display form, not its raw bytes

- GIVEN the harvested `Ge` instruction (P4 `"BINARY-8"`) and the harvested
  `String8` instruction (P4 `"g%"`)
- THEN `Ge`'s `p4` column shows `BINARY-8` and `String8`'s `p4` column
  shows `g%` — both are the same rendering the oracle itself produces via
  `EXPLAIN`, not an internal debug representation

**Tests:** `tests/vdbe/explain_test.rs::explain_p4_column_matches_oracle_display_form`

### Requirement 11: Expression Emission — Control Flow, Not Boolean Values [MUST]

Codegen MUST compile boolean-valued SQL expressions (WHERE clauses,
CASE/WHEN conditions, short-circuiting AND/OR) into jump instructions
targeting either a "true" or "false" continuation address, never into an
intermediate boolean value written to a register and then tested by a
separate opcode — this is SQLite's own emission shape (observable via
`EXPLAIN` on the pinned oracle) and follows directly from the compare
opcodes already being jump instructions (Requirement 5). `AND` MUST
short-circuit by emitting a jump to the false target on any false operand
without evaluating remaining operands; `OR` MUST short-circuit
symmetrically to the true target. `CASE` MUST compile to a jump chain: each
`WHEN` condition either falls through to its `THEN` result or jumps to the
next `WHEN` test, with a final unconditional jump past the chain after the
matching branch's result is computed.

> **Note (resolved by #91):** spike 008 (#59) has since completed;
> its kept oracle vectors (`tests/corpus/expr_vectors/walker.jsonl`)
> now run through the real compiled path
> (`tests/codegen/expr_test.rs::walker_vectors_pass_through_the_compiled_path`),
> confirming this requirement's jump-shape description. A handful of
> vectors remain documented gaps (bitwise/concat opcodes absent from
> the frozen V2 set, full three-valued NULL propagation through NOT/
> AND/OR/BETWEEN/IN in value context, CAST's lossy-conversion
> semantics, REAL-literal representation) — see that test file's
> `KNOWN_GAPS` doc comment.

**Implementation:** `src/codegen/expr.rs`, `src/codegen/select.rs` (#91)

#### Scenario: WHERE compiles to a jump past the row-handling code, not a boolean register test

- GIVEN `SELECT * FROM products WHERE price > 10` (harvested: `Gt`/`Ge`
  family opcodes jump directly out of the scan loop body on a false
  comparison — per `tools/opcodes-v2.json`'s control-flow shape)
- THEN the compiled WHERE condition is a jump instruction whose false
  target is the loop's `Next` instruction (skip this row, continue
  scanning) — there is no intermediate register holding a boolean 0/1
  that a separate opcode then branches on

**Tests:** `tests/codegen/expr_test.rs::where_clause_compiles_to_direct_jump`

#### Scenario: AND short-circuits without evaluating its second operand on a false first operand

- GIVEN `WHERE price >= 10 AND qty < 50` (harvested: `Ge` and `Lt` both
  present as separate jump instructions, per `tools/opcodes-v2.json`)
- THEN a false `Ge` result jumps directly to the row-skip target without
  ever reaching the `Lt` instruction — `qty < 50` is not evaluated when
  `price >= 10` is already false

**Tests:** `tests/codegen/expr_test.rs::and_short_circuits_on_false_first_operand`

## Traceability Note

Requirements 1, 2 (partial), 3, 4, 5 (partial), 6, 8, and 9 were made
active by #89 (VDBE core: instruction format, register file, control/
arithmetic/compare/result opcodes) and #90 (cursor, ephemeral-index, and
sorter opcode families). Requirements 7 (`Function` opcode dispatch), 10
(`EXPLAIN`), and 11 (expression emission) are now active too: #91 wired
the real SQL-to-`Program` pipeline (`src/codegen/`), the `Function`
opcode's dispatch (`src/vdbe/exec.rs`), and the `EXPLAIN` printer
(`src/vdbe/explain.rs`).

`tests/vdbe/opcode_completeness_test.rs` (#65) asserts `Opcode::ALL`
(`src/vdbe/program.rs`) exactly matches `tools/opcodes-v2.json`'s
harvested opcode set — the full 52-opcode inventory, independent of how
many are dispatched yet. `tools/assurance.py`'s `Opcode completeness:`
line tracks how many of those 52 are actually dispatched in
`src/vdbe/exec.rs` (49/52 with #90's cursor/sorter/ephemeral families
landed).
