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
by the phase-3 opener (#87), grown by #139's bitwise/concat harvest,
#142's literal-fidelity/CAST harvest (`Real`, `Blob`, `Int64`, `Cast`),
and #137's bound-parameter harvest (`Variable`, for `WHERE rowid = ?`
point lookups): the harvested, scope-decided `tools/opcodes-v2.json` (68 opcodes,
oracle 3.53.4) is the exhaustive
denominator for every requirement below — no opcode outside that inventory
is in scope for V2, and every opcode inside it must appear as a scenario
somewhere in this spec (#65 wires the count into the assurance dashboard).
Refs: 001/Req-3.

## Philosophy

SQLite has one VM, one instruction set, and one place semantics live: the
value-semantics kernel (spec 008, `src/vdbe/{compare,affinity,coerce,value}.rs`
plus `src/record/collation.rs`). The VDBE itself is a dumb dispatcher over that kernel —
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
`tests/unit/vdbe_cursor_sorter_test.rs::full_scan_program_matches_oracle_row_for_row`

#### Scenario: A rowid-alias column is read with Rowid on every path, including `*`

- GIVEN a table whose `INTEGER PRIMARY KEY` column is stored as a NULL
  placeholder in every record (spec 006 Requirement 4, which defers the
  substitution to "a higher row-assembly layer" — this is that layer)
- WHEN any of `SELECT id`, `SELECT *`, `SELECT tbl.*`, or a `WHERE`
  comparison reads that column
- THEN codegen emits `Rowid`, never `Column` — the substitution is a
  property of the column, not of the syntax that names it. Emitting
  `Column` on any one path answers NULL there while the others answer
  correctly, which is how `SELECT * FROM t` stayed wrong after
  `SELECT id FROM t` was fixed (#131, #134)

**Tests:** `tests/unit/codegen.rs::star_expansion_reads_the_rowid_alias_via_rowid`, `tests/unit/codegen.rs::rowid_alias_result_column_reads_via_rowid_not_column`, `tests/parity/v02.rs::star_expansion_acceptance_and_output_match_for_a_rowid_alias_table`

#### Scenario: `WHERE rowid = <literal or ?>` seeks directly instead of scanning

- GIVEN `SELECT * FROM products WHERE id = 2` (harvested: `Integer`,
  `SeekRowid` — per `tools/opcodes-v2.json`, #137/#128), where `id` is
  either the bare `rowid`/`_rowid_`/`oid` keyword or the table's actual
  `INTEGER PRIMARY KEY` alias column
- THEN codegen recognizes the single top-level equality against a rowid
  reference and emits `Integer` (or `Variable` for `= ?`) followed by
  `SeekRowid` directly on the table cursor, in place of the
  `Rewind`/`[test, emit]`/`Next` scan loop — an O(log n) point lookup
  instead of an O(n) full-table scan. Out of this scenario's scope
  (falls back to the ordinary scan): range comparisons (`rowid > 5`),
  compound conditions (`rowid = 5 AND x = 3`), DISTINCT, and any
  non-rowid column — those stay V4 per the issue's bounded scope

**Tests:** `tests/unit/codegen.rs::rowid_alias_equality_compiles_to_seek_rowid`, `tests/unit/codegen.rs::bare_rowid_keyword_equality_compiles_to_seek_rowid`, `tests/unit/codegen.rs::rowid_equality_against_parameter_compiles_to_seek_rowid`, `tests/unit/codegen.rs::rowid_range_comparison_does_not_use_seek_rowid`, `tests/unit/codegen.rs::non_rowid_column_equality_does_not_use_seek_rowid`, `tests/unit/codegen_select_test.rs::rowid_equality_seeks_and_matches_oracle`, `tests/unit/codegen_select_test.rs::rowid_equality_seek_missing_row_returns_empty`

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
`tests/unit/vdbe_cursor_sorter_test.rs::distinct_program_discards_rows_already_seen`

DISTINCT's NULL-equals-NULL dedup — unlike `=`, and unlike ORDER BY's
default NULL placement — is pinned separately in Requirement 9's "NULL
is comparison-distinct across `=`, DISTINCT, and ORDER BY" scenario
(#146).

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
collation dispatch to `src/record/collation.rs` (Requirement 3 there); the
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

#### Scenario: Compare opcodes apply the P4 affinity byte before comparing, derived from both operands

- GIVEN `SELECT * FROM t WHERE i = '5'` against a table with `i INTEGER`
  (#138: the oracle emits `Ne r2, →7, r1, BINARY-8, p5=84` — affinity
  folded into the compare opcode's P4/P5, applied before comparing)
- THEN codegen derives comparison affinity from both operands per
  SQLite's own rule (numeric affinity wins if either operand has one;
  a column/CAST operand's affinity wins over an operand with none;
  otherwise no affinity is applied) and `compare_jump` applies it, via
  `src/vdbe/affinity.rs`, to copies of both operands before delegating
  to `compare()` — so `i = '5'` matches the INTEGER row instead of
  falling back to NULL/numeric/text/blob storage-class ordering

**Tests:** `src/vdbe/exec.rs::tests::compare_jump_applies_comparison_affinity_derived_from_both_operands`

### Requirement 6: Arithmetic Opcodes [MUST]

The 13 arithmetic-category opcodes — `Add`, `Subtract`, `Multiply`,
`Divide`, `Remainder`, `Not`, `BitAnd`, `BitOr`, `ShiftLeft`,
`ShiftRight`, `BitNot`, `Concat`, `Cast` — MUST delegate all overflow,
NULL-propagation, and numeric/text-coercion behavior to spec 008's
`src/vdbe/coerce.rs` (Requirement 5 there: `i64` overflow promotes to
REAL, never wraps; bitwise/shift operands coerce to INTEGER, `||`
operands coerce to TEXT) and `src/vdbe/value.rs` (Requirement 4 there:
NULL propagates through arithmetic); the opcode layer supplies only
register addressing (read the source register(s), write one destination
register). `Not` is the unary member: it MUST write the boolean
complement of `P1` into `P2`, and MUST write NULL — not 1 — when `P1`
is NULL, since `NOT unknown` is unknown (#134). `BitNot` is the other
unary member: it MUST write the bitwise complement of `P1` (coerced to
INTEGER) into `P2`, and — unlike `Not` — MUST write NULL when `P1` is
NULL, since `~NULL` stays NULL rather than resolving to a definite
value (#139).

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

#### Scenario: Not complements a register's truthiness and leaves NULL as NULL

- GIVEN `SELECT NOT qty FROM products` (harvested: `Not` once — per
  `tools/opcodes-v2.json`)
- WHEN the operand register holds a falsy value, a truthy value, and NULL
  in turn
- THEN the destination register holds 1, 0, and NULL respectively — the
  NULL case is what distinguishes this opcode from the jump-mode
  compiler, which has only two continuations and must fold unknown into
  one of them

**Tests:** `src/vdbe/arithmetic.rs::tests::not_complements_truthiness_and_propagates_null`

#### Scenario: Bitwise/shift/concat opcodes coerce operands and propagate NULL

- GIVEN `SELECT qty & 1, qty | 1, qty << 1, qty >> 1, ~qty, name || note
  FROM products` (harvested: `BitAnd`, `BitOr`, `ShiftLeft`, `ShiftRight`,
  `BitNot`, `Concat`, each once — per `tools/opcodes-v2.json`, #139)
- WHEN `qty`/`name`/`note` are non-NULL, and again when one operand is
  NULL
- THEN `BitAnd`/`BitOr`/`ShiftLeft`/`ShiftRight` coerce both operands to
  INTEGER before computing, `Concat` coerces both operands to TEXT
  before concatenating, `BitNot` coerces its one operand to INTEGER
  before complementing, and every one of the six writes NULL to its
  destination register when any operand register holds NULL — matching
  the oracle table in #139 (`i & 3`, `i | 3`, `i << 1`, `i >> 1`, `~i`,
  `s || 'x'`) exactly, including negative-shift-amount and
  shift-magnitude-≥64 edge cases (SQLite's `vdbe.c` reversal/clamp rule)

**Tests:** `src/vdbe/arithmetic.rs::tests::bitwise_and_or_shift_concat_read_two_registers_write_one`,
`src/vdbe/arithmetic.rs::tests::bit_not_complements_and_propagates_null`,
`src/vdbe/arithmetic.rs::tests::null_propagates_through_bitwise_shift_and_concat`,
`src/vdbe/coerce.rs::tests::shift_handles_negative_and_oversized_amounts`,
`tests/unit/codegen_expr_test.rs::walker_vectors_pass_through_the_compiled_path`

#### Scenario: Cast forces P1's target affinity via the kernel's own CAST rule, never MustBeInt/RealAffinity

- GIVEN `SELECT CAST(name AS INTEGER), CAST(price AS REAL), CAST(qty AS
  TEXT), CAST(name AS BLOB), CAST(price AS NUMERIC) FROM products`
  (harvested: `Cast` 11 times, P1 = the register to convert in place,
  P2 = the target affinity's ASCII byte — e.g. `68`/`'D'` for INTEGER —
  P4 absent, per `tools/opcodes-v2.json`, #142)
- THEN `Cast` decodes `P2` back to an `Affinity` and delegates the
  conversion to `src/vdbe/cast.rs`'s `cast_to`, SQLite's own lossy
  `CAST` rule (`sqlite3VdbeMemCast`) — never `MustBeInt` (a guard opcode
  that aborts instead of truncating: `CAST('apple' AS INTEGER)` is `0`,
  not an error) or `RealAffinity` (a column-load coercion opcode with a
  different, narrower rule: only well-formed numeric text converts, and
  BLOB never does)

**Tests:** `src/vdbe/cast.rs::tests::cast_to_integer_matches_oracle_truth_table`,
`src/vdbe/cast.rs::tests::cast_to_real_matches_oracle_truth_table`,
`src/vdbe/cast.rs::tests::cast_to_text_matches_oracle_truth_table`,
`src/vdbe/cast.rs::tests::cast_to_blob_matches_oracle_truth_table`,
`src/vdbe/cast.rs::tests::cast_to_numeric_matches_oracle_truth_table`,
`tests/unit/codegen_expr_test.rs::walker_vectors_pass_through_the_compiled_path`

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

**Tests:** `tests/unit/codegen_expr_test.rs::like_and_glob_dispatch_through_the_function_opcode`,
`tests/unit/codegen_expr_test.rs::single_arg_function_call_compiles`,
`tests/unit/codegen_expr_test.rs::multi_arg_function_call_compiles_with_contiguous_registers`

#### Scenario: Zero-arg function call compiles with no argument registers

- GIVEN `SELECT sqlite_version() FROM t`, a registered arity-0 scalar
  function (#136)
- THEN codegen's `FunctionCall` zero-arg branch allocates a scratch
  register for P2 (never read, since the argument loop is `0..arity`)
  and emits a single `Function` instance with P4 `"sqlite_version(0)"`,
  writing the registry's return value to P3

**Tests:** `tests/unit/codegen_expr_test.rs::zero_arg_function_call_compiles`

### Requirement 8: Result-Row Opcodes [MUST]

The 10 result-category opcodes — `Integer`, `Int64`, `Real`, `Blob`,
`Null`, `String8`, `Variable`, `Copy`, `MakeRecord`, `ResultRow` — MUST implement
literal loading (`Integer`: load an `i64` constant into a register from
`P1`; `Int64`: load an `i64` constant carried in `P4` — the 64-bit
counterpart for a literal outside `P1`'s `i32` range, #142; `Real`:
load an `f64` constant from `P4`; `Blob`: load a byte-string constant
from `P4`; `Null`: write NULL into the register range `P2..=P3`, or
just `P2` when `P3` does not name a higher register; `String8`: load a
UTF-8 string constant from `P4` into a register; `Variable`: load bound
parameter `P1` (1-based, `sqlite3_bind_*` convention) into register `P2`,
reading NULL for an unbound or out-of-range index; `Copy`: `r[P2] = r[P1]`,
relocating an already-computed value into a different register), record serialization (`MakeRecord`:
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

#### Scenario: Int64/Real/Blob load a literal as its own typed Value, not text relying on coercion

- GIVEN `SELECT 3000000000, 9223372036854775807, -9223372036854775808
  FROM products LIMIT 1` (harvested: `Int64` 3 times, P4 the literal's
  decimal text), `SELECT 1e3, .5, 1. FROM products LIMIT 1` (harvested:
  `Real` 3 times, P4 the literal's decimal text), and `SELECT X'414243'
  FROM products LIMIT 1` (harvested: `Blob` once, P1 = byte length 3,
  P4 the raw bytes rendered as text `"ABC"` — per `tools/opcodes-v2.json`,
  #142)
- THEN `Int64` writes its `P4`-carried `i64` into its destination
  register (closing the gap where a literal outside `Opcode::Integer`'s
  `i32`-only `P1` was a hard codegen error), `Real` writes its
  `P4`-carried `f64` as an actual `Value::Real` (not `String8` text —
  the #138 comparison-affinity bug this used to cause), and `Blob`
  writes its `P4`-carried bytes as an actual `Value::Blob` (not
  `String8` hex text, which BLOB affinity never converts back — the
  reason `WHERE b = x'41'` always returned zero rows before this)

**Tests:** `src/vdbe/result.rs::tests::int64_real_and_blob_load_typed_literals`,
`tests/unit/codegen_expr_test.rs::walker_vectors_pass_through_the_compiled_path`

#### Scenario: Variable loads a bound parameter, defaulting to NULL when unbound

- GIVEN `SELECT * FROM products WHERE id = ?1` (harvested: `Variable`
  once — per `tools/opcodes-v2.json`, #137)
- THEN `Variable` reads the parameter at its 1-based `P1` index from
  whatever `Vm::bind_params`/`execute_with_params` bound before the run
  started, and writes it into register `P2`; an index past the end of
  the bound-value list (including "nothing was ever bound") writes NULL
  rather than erroring — consistent with the rest of the VM's
  unwritten-register-reads-as-NULL rule

**Tests:** `tests/unit/codegen_select_test.rs::rowid_equality_against_bound_parameter_seeks`

#### Scenario: Null writes NULL over a register that already holds a value

- GIVEN `SELECT CASE WHEN price > 100 THEN 1 END FROM products`
  (harvested: `Null` once — per `tools/opcodes-v2.json`), whose
  no-branch-matched result register is reused on every scan iteration
- THEN `Null` writes NULL into `P2` (through `P3` when that names a
  higher register), overwriting whatever the register held — an
  unwritten register is not a substitute, because it cannot express a
  NULL that has to replace a live value

**Tests:** `src/vdbe/result.rs::tests::null_overwrites_a_live_register_and_spans_p2_to_p3`

#### Scenario: Copy relocates a computed value into a shared or reserved register

- GIVEN `SELECT count(*), sum(price) FROM products` (harvested: `Copy`
  once — per `tools/opcodes-v2.json`, #141) and, in this crate's own
  codegen, two result columns that each allocate temporaries before
  their own destination register (e.g. `SELECT i + 1, i - 1 FROM t`),
  or a CASE branch that is a compound expression rather than a bare
  literal/column reference
- THEN `Copy` writes `r[P1]` into `r[P2]`, letting `compile_row_values`/
  `emit_branch_into` compute a value wherever the bump allocator lands
  it and then relocate it into the contiguous run `MakeRecord`/
  `ResultRow` need (or into a CASE branch's shared result register)
  instead of refusing the query outright

**Tests:** `tests/unit/codegen_expr_test.rs::case_branch_with_computed_expression_compiles_via_copy`,
`tests/unit/codegen_select_test.rs::two_computed_result_columns_do_not_collide`

#### Scenario: ResultRow emits a fixed register range as one output row every iteration

- GIVEN `SELECT * FROM products` (harvested: `ResultRow` 26 times — once
  per statement run across the harvest's 26-query set, one row-emit call
  site per query regardless of row count, per `tools/opcodes-v2.json`)
- THEN `ResultRow` yields the registers in its `[P1, P1+P2)` range as one
  logical output row to the statement's caller, without itself advancing
  any cursor or looping — looping is the scan opcodes' job (Requirement 4)

**Tests:** `src/vdbe/result.rs::tests::result_row_emits_fixed_register_range`

#### Scenario: A FROM-less SELECT emits exactly one row with no cursor/scan (#260)

- GIVEN `SELECT sqlite_version()` — no `FROM` clause at all, SQLite's
  normal way to call a zero-arg built-in
- THEN codegen compiles the column list once against an empty schema
  and emits a single `ResultRow` with no `OpenRead`/scan-loop
  bracketing it; `*`/`tbl.*` and any clause presuming a table
  (WHERE/GROUP BY/HAVING/ORDER BY/LIMIT/DISTINCT/compound) is rejected
  as unsupported rather than silently no-op'd

**Tests:** `tests/unit/codegen_expr_test.rs::from_less_select_compiles_a_bare_expression_list`,
`tests/unit/codegen_expr_test.rs::from_less_select_rejects_star`

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
`tests/unit/vdbe_cursor_sorter_test.rs::order_by_program_emits_rows_in_sorted_order`

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

#### Scenario: ORDER BY terms resolve ordinals and result-column aliases, not just bare columns (#144)

- GIVEN `SELECT a, b FROM t ORDER BY 2 DESC` (an ordinal) and
  `SELECT a, b AS x FROM t ORDER BY x DESC` (a result-column alias)
- THEN both resolve to the same underlying table column
  `resolve_order_by` would use for a bare `ORDER BY b` — no new
  `SortKeyColumn`/sorter-record shape is needed, since ordinals and
  aliases-of-columns both bottom out at a raw table column index — and
  an out-of-range ordinal or an unresolvable alias is rejected the same
  way an unknown bare column name already is
**Tests:** `tests/unit/codegen_select_test.rs::order_by_ordinal_resolves_result_column`,
`tests/unit/codegen_select_test.rs::order_by_alias_resolves_result_column`

#### Scenario: ORDER BY terms may be genuine expressions, not just columns/ordinals/aliases (#155)

- GIVEN `SELECT a FROM t ORDER BY -a` (a unary expression),
  `SELECT a, b FROM t ORDER BY b - a` (binary), `SELECT name FROM t
  ORDER BY lower(name) DESC` (a scalar function call), or
  `SELECT -a AS neg FROM t ORDER BY neg` (an alias whose own result
  expression is computed rather than a bare column)
- THEN `compile_sorted_scan` computes the term into its own register,
  appended after the raw schema-column block already fed to
  `MakeRecord`/`SorterInsert` — `SortKeyColumn.index` points at that
  register's position in the record, resolved once pass 1's codegen
  has actually allocated registers (the `SorterOpen` `P4::SortKey`
  descriptor is patched in after the fact rather than computed
  up-front) — and the sorted output matches the oracle exactly,
  including in combination with LIMIT/OFFSET and a second, plain
  sort key
- The final `ResultRow` projection (`projection::emit_row_via_sink`) is unaffected:
  it only ever reads `select.columns` from the pseudo-cursor's
  decoded record, so the trailing sort-key-only registers are never
  projected

**Tests:** `tests/unit/codegen_select_test.rs::order_by_unary_expression_matches_oracle`,
`tests/unit/codegen_select_test.rs::order_by_binary_expression_matches_oracle`,
`tests/unit/codegen_select_test.rs::order_by_function_call_matches_oracle`,
`tests/unit/codegen_select_test.rs::order_by_alias_to_computed_expression_matches_oracle`,
`tests/unit/codegen_select_test.rs::order_by_expression_with_limit_offset_and_second_key`

#### Scenario: NULL is comparison-distinct across `=`, DISTINCT, and ORDER BY (#146)

- GIVEN a column containing NULL alongside non-NULL values, with no
  explicit `NULLS FIRST`/`NULLS LAST` in the `ORDER BY` (that override is
  Requirement 9's own `SorterOpen`-descriptor mechanism, unaffected by
  this scenario)
- THEN `=` (spec 008 Requirement 4's three-valued logic) treats two NULLs
  as UNKNOWN, never equal; DISTINCT's ephemeral-index dedup (Requirement
  4 of this spec) treats two NULLs as equal via exact record-byte
  equality, collapsing them to one row; and the sorter's default (no
  `NULLS` clause) places NULL first on `ASC` and last on `DESC` — three
  independent rules for the same value, none derivable from another, so
  a refactor unifying these comparison paths must preserve all three

**Tests:** `src/vdbe/sorter.rs::tests::ascending_default_places_nulls_first`,
`src/vdbe/sorter.rs::tests::descending_default_places_nulls_last`,
`src/vdbe/cursor.rs::tests::distinct_treats_two_nulls_as_equal_unlike_the_eq_operator`

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

**Tests:** `tests/unit/vdbe_explain_test.rs::explain_renders_one_row_per_instruction_all_columns`

#### Scenario: EXPLAIN's p4 column renders the operand's display form, not its raw bytes

- GIVEN the harvested `Ge` instruction (P4 `"BINARY-8"`) and the harvested
  `String8` instruction (P4 `"g%"`)
- THEN `Ge`'s `p4` column shows `BINARY-8` and `String8`'s `p4` column
  shows `g%` — both are the same rendering the oracle itself produces via
  `EXPLAIN`, not an internal debug representation

**Tests:** `tests/unit/vdbe_explain_test.rs::explain_p4_column_matches_oracle_display_form`

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

Because SQL is three-valued and a jump has two destinations, the
jump-mode entry point MUST also carry which continuation the *unknown*
outcome joins (`NullTarget`, SQLite's own `jumpIfNull` flag). `WHERE`
and `CASE WHEN` MUST pass "unknown joins false" — a predicate whose
truth is unknown excludes the row exactly like a false one. Any
lowering that exchanges the true and false continuations — `NOT`, and
`<>` as an `Eq` with the targets swapped — MUST flip that setting with
them, so the unknown outcome stays on the address it already had.
Materializing a condition into a register (result columns) MUST be able
to produce NULL, not only 0/1.

> **Note (resolved by #91, amended by #134):** spike 008 (#59) has since
> completed; its kept oracle vectors
> (`tests/corpus/expr_vectors/walker.jsonl`) now run through the real
> compiled path
> (`tests/unit/codegen_expr_test.rs::walker_vectors_pass_through_the_compiled_path`),
> confirming this requirement's jump-shape description. #134 added the
> `NullTarget` clause above and the `Null`/`Not` opcodes behind it,
> retiring the NOT/AND/OR/BETWEEN/IN-over-NULL vectors from that test's
> `KNOWN_GAPS`. The gaps that remain are unrelated to three-valued
> logic (bitwise/concat opcodes absent from the frozen V2 set, CAST's
> lossy-conversion semantics, REAL-literal representation, `LIKE ...
> ESCAPE` operand ordering) — see that test file's `KNOWN_GAPS` doc
> comment.

**Implementation:** `src/codegen/expr.rs`, `src/codegen/select.rs` (#91)

#### Scenario: WHERE compiles to a jump past the row-handling code, not a boolean register test

- GIVEN `SELECT * FROM products WHERE price > 10` (harvested: `Gt`/`Ge`
  family opcodes jump directly out of the scan loop body on a false
  comparison — per `tools/opcodes-v2.json`'s control-flow shape)
- THEN the compiled WHERE condition is a jump instruction whose false
  target is the loop's `Next` instruction (skip this row, continue
  scanning) — there is no intermediate register holding a boolean 0/1
  that a separate opcode then branches on

**Tests:** `tests/unit/codegen_expr_test.rs::where_clause_compiles_to_direct_jump`

#### Scenario: AND short-circuits without evaluating its second operand on a false first operand

- GIVEN `WHERE price >= 10 AND qty < 50` (harvested: `Ge` and `Lt` both
  present as separate jump instructions, per `tools/opcodes-v2.json`)
- THEN a false `Ge` result jumps directly to the row-skip target without
  ever reaching the `Lt` instruction — `qty < 50` is not evaluated when
  `price >= 10` is already false

**Tests:** `tests/unit/codegen_expr_test.rs::and_short_circuits_on_false_first_operand`

#### Scenario: NOT over a comparison keeps the unknown outcome excluding the row

- GIVEN `SELECT * FROM products WHERE NOT (price = 10)` over a row whose
  `price` IS NULL (the pinned oracle emits a single `Eq` jump with
  `SQLITE_JUMPIFNULL` set in P5, sending both the equal and the unknown
  outcome to the row-skip target)
- WHEN the negation swaps the true and false continuations
- THEN the unknown outcome still reaches the row-skip target, so the
  NULL row is excluded — this instruction format carries no P5 flag, so
  the jump-if-null is spelled as explicit `IsNull` operand probes ahead
  of the compare, which a bare target swap does not emit

**Tests:** `tests/unit/codegen.rs::not_over_a_comparison_probes_for_null_instead_of_swapping_targets`, `tests/unit/codegen.rs::ne_probes_for_null_like_a_negated_eq`, `tests/parity/v02.rs::three_valued_logic_acceptance_and_output_match_over_null_rows`

#### Scenario: NOT (x IN (...)) and x NOT IN (...) compile to the same program

- GIVEN the two spellings of the same condition, over a fixture with
  NULL rows
- THEN they emit instruction-for-instruction identical programs, because
  `NOT`'s flipped null continuation resolves to the same address that
  `NOT IN`'s own unknown path already used — two spellings of one
  condition must not return different rows

**Tests:** `tests/unit/codegen.rs::not_in_and_in_negated_compile_to_the_same_program`

#### Scenario: A condition in value context materializes true, false, or NULL

- GIVEN `SELECT NOT qty FROM products` and `SELECT price = 10 FROM products`
  over a row whose operand IS NULL
- THEN the result register holds NULL, not 0 or 1 — `NOT` lowers to the
  `Not` opcode (Requirement 6), and a comparison lowers to a jump-mode
  test whose unknown branch writes the `Null` opcode (Requirement 8)

**Tests:** `tests/unit/codegen.rs::not_in_value_context_uses_the_not_opcode`, `tests/unit/codegen.rs::comparison_in_value_context_materializes_three_outcomes`, `tests/unit/codegen_expr_test.rs::walker_vectors_pass_through_the_compiled_path`

### Requirement 12: Aggregate Opcodes [MUST]

`AggStep` and `AggFinal` MUST NOT contain any aggregate-specific logic
themselves — same no-VDBE-layer-logic discipline as `Function`
(Requirement 7) — dispatching instead into a shared aggregate registry
(`src/vdbe/aggregate.rs`) by a P4 descriptor: `AggFinal` uses a plain
`"name(arity)"` string (arity unused, since finalizing performs no
comparison); `AggStep` uses `P4::AggFunc { name, arity, collation }`
(#263, ADR-0019) so `min`/`max` compare under the aggregated argument's
declared collation instead of always BINARY. `AggStep` folds a
contiguous run of argument registers (starting at `P2`) into an
aggregate-context slot addressed by `P1`, creating a fresh accumulator
on that slot's first `AggStep` — or on any `AggStep` whose `P5` is
nonzero, which discards the slot's prior state before folding (#263's
mechanism for starting a new `GROUP BY` group on a reused slot number,
without a dedicated reset opcode). `AggFinal` reads context slot `P1`
and writes the finalized result to register `P3`, without erroring
when the slot was never stepped — an empty group is a legitimate
zero-row result (`count` finalizes to 0, `sum` to NULL), not a
malformed program. Both opcodes are part of the harvested/frozen
`Opcode::ALL` set (ADR-0018) — the "postdate the V2 harvest, excluded
from `Opcode::ALL`" state this requirement originally described was
superseded when ADR-0018's re-harvest added them.

`OpenEphemeral`'s existing in-memory ephemeral-table support (Requirement
4) is reused as the GROUP BY grouping-table backing store — no new
cursor machinery is introduced by this requirement.

**Implementation:** `src/vdbe/exec.rs::agg_step`, `src/vdbe/exec.rs::agg_final`,
`src/vdbe/aggregate.rs` (registry: `count`/`sum`/`avg`/`min`/`max`),
`src/codegen/select/aggregate/accum.rs::emit_agg_step` (GROUP BY/plain-aggregate codegen, #263)

#### Scenario: AggStep accumulates across repeated calls into the same context slot

- GIVEN a context slot never stepped, then `AggStep` run three times with
  `P4 = "sum(1)"` over registers holding 10, 20, 30
- THEN `AggFinal` on that slot with the same descriptor writes 60 to its
  result register

**Tests:** `src/vdbe/exec.rs::tests::agg_step_accumulates_across_repeated_calls_into_the_same_context_slot`

#### Scenario: AggFinal on a never-stepped slot yields the aggregate's zero-row result

- GIVEN a context slot with no prior `AggStep` call
- THEN `AggFinal` with `P4 = "count(0)"` writes 0, and `P4 = "sum(1)"`
  writes NULL — an empty group is a valid outcome, not an error

**Tests:** `src/vdbe/exec.rs::tests::agg_final_on_a_never_stepped_slot_yields_the_zero_row_result`

#### Scenario: Distinct aggregate-context slots do not alias

- GIVEN `AggStep("count(1)")` run once against slot 0 and twice against
  slot 1
- THEN `AggFinal` on slot 0 yields 1 and on slot 1 yields 2 — slots are a
  disjoint address space, the same shape as cursor slots (Requirement 2)

**Tests:** `src/vdbe/exec.rs::tests::distinct_agg_context_slots_do_not_alias`

#### Scenario: AggStep's P5 discards prior state before folding

- GIVEN a context slot already stepped once with `sum(1)`
- WHEN `AggStep` runs again with `P5` nonzero
- THEN the slot's prior state is discarded first — `AggFinal` reads only
  the post-reset call's contribution, not the sum of both

**Tests:** `src/vdbe/exec.rs::tests::agg_step_with_nonzero_p5_discards_prior_state_before_folding`

#### Scenario: AggStep's min/max compare under the P4 collation

- GIVEN `AggStep("min(1)")` with `P4::AggFunc`'s `collation` set to
  `NOCASE`, run over the text values `'B'` then `'a'`
- THEN `AggFinal` yields `'a'` — under BINARY, `'B'` (ASCII 66) would
  have stayed the minimum since it sorts below every lowercase letter

**Tests:** `src/vdbe/exec.rs::tests::agg_step_min_honours_a_nocase_collation`,
`tests/unit/codegen_select_test.rs::min_max_aggregate_honours_collate_nocase`

### Requirement 13: Non-Recursive CTE Materialization [MUST]

A non-recursive `WITH` clause (#375's parser support) MUST be rewritten
away before the rest of codegen runs, rather than given its own
materialization path: `codegen::expand_with_clause` replaces every
`FROM`/`JOIN` table reference that names a CTE with a
`TableRefKind::Subquery` wrapping that CTE's own query, reusing
Requirement 4's existing `OpenEphemeral`-backed `FROM`-subquery
materialization unchanged. A CTE name shadows a same-named real table
for the scope of the one `SELECT` that declared it — the rewrite is a
local AST transformation, never a catalog mutation, so it cannot leak
into a sibling statement. Later CTEs in the same `WITH` list may
reference an earlier one by name (non-recursively); an explicit
`WITH cte(a, b) AS (...)` column list renames that CTE's exposed output
columns positionally. `WITH RECURSIVE` stays out of scope (rejected by
the parser, #375).

**Implementation:** `src/codegen/subquery/cte.rs::expand_with_clause`,
`src/codegen/subquery/from_clause.rs::materialize_from_subquery`

#### Scenario: A CTE referenced in FROM materializes and scans like any table

- GIVEN `WITH cte AS (SELECT id, x FROM t WHERE x > 15) SELECT * FROM cte`
- THEN `cte`'s query is materialized into an ephemeral table and the
  main query scans it, yielding the same rows a real table with those
  contents would

**Tests:** `tests/corpus/cte_test.rs::with_clause_single_cte_matches_oracle`, `tests/corpus/cte_test.rs::with_clause_cte_referenced_twice_self_join_matches_oracle`, `tests/corpus/cte_test.rs::with_clause_cte_with_internal_order_by_limit_matches_oracle`

#### Scenario: An explicit CTE column list renames its output columns

- GIVEN `WITH cte(a, b) AS (SELECT id, x FROM t) SELECT a, b FROM cte`
- THEN `a`/`b` resolve to `cte`'s query's first/second projected column
  respectively

**Tests:** `tests/corpus/cte_test.rs::with_clause_explicit_column_list_matches_oracle`

#### Scenario: A CTE joined against another table, further filtered by WHERE

- GIVEN `WITH cte AS (SELECT id, x FROM t) SELECT ... FROM cte JOIN other
  ON ... WHERE cte.x < 25`
- THEN the join and the `WHERE` filter both resolve against the
  materialized `cte` cursor exactly as they would against a real table

**Tests:** `tests/corpus/cte_test.rs::with_clause_cte_joined_and_filtered_matches_oracle`

#### Scenario: A later CTE in the same WITH list references an earlier one

- GIVEN `WITH a AS (SELECT id, x FROM t WHERE x > 10), b AS (SELECT *
  FROM a WHERE x < 30) SELECT * FROM b`
- THEN `b`'s own materialization scans `a`'s already-rewritten query
  (nested `FROM`-subquery materialization), not a catalog table named
  `a`

**Tests:** `tests/corpus/cte_test.rs::with_clause_second_cte_references_first_matches_oracle`

#### Scenario: A CTE whose body is a compound (UNION) SELECT is rejected cleanly

- GIVEN `WITH cte AS (SELECT x FROM t WHERE x > 15 UNION SELECT x FROM t
  WHERE x < 25) SELECT * FROM cte`
- THEN compilation MUST fail with `CodegenError::Unsupported`, not
  silently scan only the CTE body's `first` arm (a real data-loss bug
  found and fixed by #382 — `materialize_from_subquery` previously
  ignored every `compound` arm past `first`)

**Tests:** `tests/corpus/cte_test.rs::with_clause_cte_body_is_union_is_rejected_cleanly`

### Requirement 14: Compound SELECT (UNION / UNION ALL) [MUST]

A compound `SELECT`'s `first` arm and every `select.compound` arm each
get their own `OpenRead`/scan/`ResultRow` block, with cursor numbers
offset by `ScanCursors::for_arm` (4 cursors per arm) so no arm's own
sort/pseudo/DISTINCT cursor collides with another arm's. `UNION ALL`
(#240) concatenates every arm's rows with no deduplication. Plain
`UNION` (#377/#378) additionally routes every row from every arm
through one ephemeral index (`OpenEphemeral`) opened once for the whole
statement, past the last arm's own cursor block — a `Found`/`IdxInsert`
check before each `ResultRow`, reusing the exact dedup mechanism
Requirement 8's `SELECT DISTINCT` already performs
(`projection::emit_dedup_check`, factored out of
`projection::emit_distinct_guard`) — drops a row already seen from an
earlier arm instead of re-emitting it. Mixing `UNION` and `UNION ALL`
arms in one statement is simplified to "any `UNION` arm dedups the
whole result" rather than SQLite's pairwise left-to-right operator
semantics — a documented narrowing, not the general case. Every arm
must project the same number of result columns as `first` — checked at
compile time via `select_result_column_count` and reported as
`CodegenError::CompoundColumnMismatch`, never silently
padded/truncated. `ORDER BY`/`LIMIT` trailing the whole compound
statement is supported (sorts/bounds the combined result, not just the
last arm) — but an `ORDER BY` term must be an output column name/alias
or an ordinal, never a genuine expression, since a compound statement
has no table scope left once its arms are combined (matching real
SQLite's own rejection of this). Joins/subqueries within an arm, and
`INTERSECT`/`EXCEPT` (unsupported at the parser level, #377), remain out
of scope.

**Implementation:** `src/codegen/select/entry.rs::compile_select_compound`,
`src/codegen/select/projection.rs::emit_dedup_check`

#### Scenario: UNION ALL concatenates without deduplication

- GIVEN `SELECT a FROM t1 UNION ALL SELECT a FROM t1` over a two-row `t1`
- THEN every row from both arms is emitted, duplicates included

**Tests:** `tests/corpus/union_test.rs::union_all_concatenates_without_deduplication`, `tests/corpus/union_test.rs::union_all_keeps_duplicate_rows`, `tests/corpus/union_test.rs::multiple_union_all_arms_chain`, `tests/corpus/union_test.rs::where_clause_filters_only_its_own_arm`, `tests/corpus/union_test.rs::union_all_does_not_coerce_between_mismatched_arm_types`, `tests/corpus/union_test.rs::union_vs_union_all_row_counts_differ_on_same_overlapping_inputs`

#### Scenario: UNION deduplicates rows across arms

- GIVEN `SELECT a FROM t1 UNION SELECT a FROM t1` over a two-row `t1`
  with no duplicate rows within either arm alone
- THEN each distinct row is emitted exactly once, even though it
  appears in both arms

**Tests:** `tests/corpus/union_test.rs::union_dedups_duplicate_rows`, `tests/corpus/union_test.rs::union_basic_no_duplicates`, `tests/corpus/union_test.rs::union_does_not_coerce_between_mismatched_arm_types`, `tests/corpus/union_test.rs::three_way_union_chain_dedups_across_all_arms`

#### Scenario: A compound arm's column-count mismatch is rejected

- GIVEN `SELECT a FROM t1 UNION [ALL] SELECT a, b FROM t2` (arm projects
  two columns against `first`'s one)
- THEN compilation fails with `CompoundColumnMismatch`, not a
  padded/truncated row

**Tests:** `tests/corpus/union_test.rs::column_count_mismatch_is_rejected`, `tests/corpus/union_test.rs::union_column_count_mismatch_is_rejected`

#### Scenario: ORDER BY/LIMIT trailing the whole compound statement sorts/bounds the combined result

- GIVEN `SELECT a FROM t1 UNION SELECT b FROM t2 ORDER BY a DESC` (also
  covering `UNION ALL` with `LIMIT`, an ordinal `ORDER BY` term, and
  `LIMIT`/`OFFSET` together)
- THEN the whole compound result is sorted/bounded accordingly, not just
  the last arm

**Tests:** `tests/corpus/union_test.rs::union_with_trailing_order_by_matches_oracle`, `tests/corpus/union_test.rs::union_all_with_order_by_and_limit_matches_oracle`, `tests/corpus/union_test.rs::union_with_order_by_ordinal_and_limit_offset_matches_oracle`, `tests/corpus/union_test.rs::union_all_with_limit_only_matches_oracle`

#### Scenario: An ORDER BY expression term on a compound statement is rejected

- GIVEN `SELECT a FROM t1 UNION SELECT b FROM t2 ORDER BY -a` (a genuine
  expression, not a bare output column name/alias or an ordinal)
- THEN compilation fails cleanly rather than silently sorting by
  something else or ignoring the term

**Tests:** `tests/corpus/union_test.rs::union_with_order_by_expression_is_rejected_cleanly`

### Requirement 15: View Storage and Query Expansion [MUST]

`CREATE VIEW` (#379's parser support) MUST register a `sqlite_master`
row exactly like `CREATE TABLE`/`CREATE INDEX` do — `type = 'view'`, `name`/`tbl_name` the view's name,
`rootpage = 0` (a view owns no b-tree of its own, matching stock
SQLite's own storage convention), `sql` the verbatim `CREATE VIEW ...`
source text — via a single `Opcode::CreateView` instruction
(`P4::CreateTable`'s `{ name, sql }` payload, reused rather than
duplicated since the two opcodes' payloads are identical). Unlike a CTE
(Requirement 13, a pure in-query AST rewrite with no persistent state),
a view's definition MUST survive being reloaded from `sqlite_master` by
a fresh connection: `schema::read_views` decodes every `type = 'view'`
row into a `ViewSchema { name, sql }`, and `codegen::resolve_views` then
parses each one's `sql` back into a `CreateView` AST via #379's parser.
`codegen::expand_views` mirrors `expand_with_clause`'s exact rewrite
shape — every `FROM`/`JOIN` table reference naming a catalog view
becomes a `TableRefKind::Subquery` wrapping that view's stored query,
reusing Requirement 4's `OpenEphemeral`-backed `FROM`-subquery
materialization unchanged — except it resolves against the always-fully-
known view catalog rather than an in-declaration-order CTE list, and
therefore recurses into any nested `TableRefKind::Subquery` (bounded by a view-name stack that rejects
cycles with `CodegenError::CircularView`) so a view-of-view resolves to arbitrary depth. An
explicit `CREATE VIEW v(a, b) AS ...` column list renames the view's
exposed output columns via the same `apply_column_aliases` helper
Requirement 13's CTE column-list rename already uses. `expand_views`
runs after `expand_with_clause` in `compile_select_program` so it also
reaches into any `TableRefKind::Subquery` the CTE rewrite just produced
(a CTE body that itself references a view), and so that a CTE shadows a
same-named view for the scope of its declaring `SELECT`, matching how a
CTE already shadows a same-named real table. `DROP VIEW` is parsed
(#379) but not yet compiled — out of scope here.

**Implementation:** `src/codegen/ddl/create_view.rs::compile_create_view`,
`src/vdbe/cursor.rs::create_view`, `src/schema/ddl_reader.rs::read_views`,
`src/codegen/subquery/views.rs::{expand_views, resolve_views}`

#### Scenario: CREATE VIEW registers a sqlite_master row with rootpage 0

- GIVEN `CREATE VIEW v AS SELECT id, x FROM t WHERE x > 15`
- WHEN executed
- THEN `sqlite_master` gains a row with `type = 'view'`, `rootpage = 0`,
  and `sql` equal to the verbatim statement text

**Tests:** `tests/corpus/view_test.rs::create_view_persists_across_reload`

#### Scenario: A view referenced in FROM expands and scans like any table

- GIVEN the view above and `SELECT * FROM v`
- THEN `v`'s stored query is materialized into an ephemeral table and
  the main query scans it, yielding the same rows a real table with
  those contents would

**Tests:** `tests/corpus/view_test.rs::create_view_simple_matches_oracle`, `tests/corpus/view_test.rs::with_clause_cte_selects_from_view_matches_oracle`

#### Scenario: An explicit view column list renames its output columns

- GIVEN `CREATE VIEW v (a, b) AS SELECT id, x FROM t` and `SELECT a, b
  FROM v`
- THEN `a`/`b` resolve to `v`'s query's first/second projected column
  respectively

**Tests:** `tests/corpus/view_test.rs::create_view_explicit_column_list_matches_oracle`

#### Scenario: A view of a view (nested views) resolves to arbitrary depth

- GIVEN `CREATE VIEW v1 AS SELECT id, x FROM t WHERE x > 10`, `CREATE
  VIEW v2 AS SELECT id, x FROM v1 WHERE x < 30`, and `SELECT * FROM v2`
- THEN `v2`'s expansion recurses into `v1`'s own `FROM` reference,
  yielding the same rows as evaluating both filters against `t` directly

**Tests:** `tests/corpus/view_test.rs::create_view_of_view_matches_oracle`

#### Scenario: A view joined against another table, further filtered by WHERE

- GIVEN a view `v` and `SELECT ... FROM v JOIN other ON ... WHERE v.x <
  25`
- THEN the join and the `WHERE` filter both resolve against the
  materialized `v` cursor exactly as they would against a real table

**Tests:** `tests/corpus/view_test.rs::create_view_joined_and_filtered_matches_oracle`

#### Scenario: DROP VIEW fails cleanly rather than being silently ignored

- GIVEN `DROP VIEW v` run against a real connection, with `DROP VIEW`
  parsed (#379) but not dispatched to any codegen path (this
  requirement's own out-of-scope note above)
- THEN the statement MUST be rejected with a clean error (`statement
  dispatch` reports it as unrecognized) rather than panicking or
  silently no-opping, and `v` remains queryable afterward — #382
  verified this end-to-end and found the existing behavior already
  clean (no codegen path is reached at all: `compile_statement`'s
  keyword dispatch has no `"DROP" if kw(1) == "VIEW"` arm, so it falls
  through to `DispatchError::Unrecognized` before ever touching a
  parser or opcode). Wiring an actual `Opcode::DropView` remains a
  fast-follow, tracked separately from this scenario.

**Tests:** `tests/corpus/view_test.rs::drop_view_fails_cleanly_not_wired_into_codegen`

### Requirement 16: Covering-Index Scan and Index-Only COUNT(*) [MUST]

Two "always wins, no ANALYZE/cost model needed" optimizations (#444).
Real SQLite has no separate index-column-read opcode — it reuses plain
`Column` against an index cursor's current entry exactly as it does
against a table cursor, and this codebase follows suit rather than
inventing a nonexistent opcode:

- **Covering-index scan**: when a single-table `SELECT`'s `WHERE`
  clause is a single top-level equality between a `UNIQUE` index's
  leading column and a literal/bind-parameter operand, and every result
  column the `SELECT` list needs (bare columns only) is itself carried
  by that same index, `try_compile_covering_index_scan` emits
  `SeekIndexEq` (the point probe) + one `Column` read per result
  column straight off the index cursor, never opening/seeking the table
  cursor at all.
  `find_covering_index` is the shared detection function both this
  codegen path and `EXPLAIN QUERY PLAN` (`SEARCH ... USING COVERING
  INDEX ...`) call, so the two can never drift apart.
- **Index-only `COUNT(*)`**: `try_compile_index_only_count` recognizes a
  bare `SELECT count(*) FROM t` (no `GROUP BY`/`HAVING`/`DISTINCT`/
  `ORDER BY`/`LIMIT`) and counts by walking any one index's b-tree
  entry-for-entry (`IdxRewind`/`IdxNext`, one entry per table row
  regardless of that index's own column values) when there's no
  `WHERE` clause, or by a single `SeekIndexEq` probe against a `UNIQUE`
  index's leading column (count is trivially 0 or 1) when `WHERE
  indexed_col = <literal/param>` is present — either way, the table
  cursor is never opened.

Both fast paths are deliberately narrow, matching this module's
existing rowid-seek/index-ordered-scan conventions: a non-equality or
multi-column `WHERE`, `*`/`table.*`/a computed result-column
expression, or (for `COUNT`) a non-unique-index equality `WHERE` all
fall back to the ordinary `Rewind`/`Next` scan or `compile_grouped_scan`
respectively, rather than risk misprojecting or miscounting duplicate
index entries this MVP's `SeekIndexEq`-based probe can't walk past.
LIMIT's own early-out (`emit_limit_guard`, already jumping to
`end_label` the moment `LIMIT`'s budget hits zero — see
`compile_direct_scan`/`try_compile_index_ordered_scan`) and the
sorter's top-K bound (`compile_sorted_scan`'s `bound_reg`/`SorterOpen`
`P5`, #129) already stop scanning as soon as enough rows are known, so
#444's third example (`ORDER BY x LIMIT 10`) needed no further codegen
change here — this requirement's `Tests:` links below only cover the
two genuinely new fast paths.

**Implementation:**
`src/vdbe/cursor.rs::read_row_column` (index-cursor case),
`src/codegen/select/limit_scan.rs::{find_covering_index, try_compile_covering_index_scan}`,
`src/codegen/select/aggregate.rs::try_compile_index_only_count`,
`src/codegen/select/eqp.rs::explain_query_plan`

#### Scenario: A covering-index equality SELECT skips the table row entirely

- GIVEN `CREATE UNIQUE INDEX idx_ab ON t(a, b)` and `SELECT a, b FROM t
  WHERE a = 6`
- THEN the compiled program contains `SeekIndexEq` but no `SeekRowid`,
  and its output matches a full table scan of the same query

**Tests:** `tests/corpus/no_stats_optimizations_test.rs::covering_index_equality_select_matches_oracle`, `tests/corpus/no_stats_optimizations_test.rs::covering_index_equality_select_miss_matches_oracle`

#### Scenario: COUNT(*) over an indexed table never decodes a table row

- GIVEN an indexed table `t` and `SELECT count(*) FROM t`, or `SELECT
  count(*) FROM t WHERE indexed_col = <literal>` against a `UNIQUE`
  index
- THEN the compiled program walks/probes the index cursor
  (`IdxRewind`/`SeekIndexEq`) and never opens a `Rewind` table scan,
  yielding the same count as the oracle

**Tests:** `tests/corpus/no_stats_optimizations_test.rs::index_only_count_star_no_where_matches_oracle`, `tests/corpus/no_stats_optimizations_test.rs::index_only_count_star_equality_where_matches_oracle`

### Requirement 17: Hash-Based GROUP BY Aggregation [SHOULD]

`GROUP BY` MUST have a hash-based execution strategy alongside
Requirement 9's sort-based one (`src/codegen/select/aggregate.rs::compile_grouped_scan`),
selected when no covering index already produces group-ordered rows.
The sort strategy pays O(n log n) to make a group's rows adjacent before
folding them; the hash strategy folds each row into its group's
accumulators as the scan reaches it, so the build is O(n) in the row
count and only the K groups are ever ordered. The sort strategy MUST
remain fully working as the fallback — it is the general path (spec 001
Tier 3, "simplifiable, not droppable"), and the hash strategy is an
addition, never a replacement.

The strategy is expressed as its own six-opcode family shaped
deliberately like the `Sorter*` family it stands beside: `HashAggOpen`
(keyed by `P4::GroupKey`, a per-key-column collation-plus-comparison-affinity
descriptor), `HashAggFind` (locate-or-create this row's group),
`HashAggStep` (fold into the located group's accumulator slot — the
per-group counterpart of Requirement 12's `AggStep`, delegating to the
same `src/vdbe/aggregate.rs` registry so an aggregate cannot mean one
thing under each strategy), `HashAggRewind`/`HashAggData`/`HashAggNext`
(iterate the groups, mirroring `SorterSort`/`SorterData`/`SorterNext`).
`HashAggData` additionally installs its group's accumulators into the
`AggStep`/`AggFinal` context slots, so the per-group flush codegen
(`flush_group`) — and with it `HAVING`, `LIMIT`, and projection — is
shared verbatim between the two strategies rather than duplicated.

Two properties MUST hold, both about not diverging observably from the
sort strategy:

- **Group identity.** Two key values MUST hash to the same group exactly
  when Requirement 5's `compare` calls them equal under that key
  column's collation, after that column's comparison affinity has been
  applied — the same collation and affinity the sort strategy carries on
  its group-boundary `Eq`. This includes SQLite's merged numeric class
  (`1` and `1.0` are one group) and collation folding (`NOCASE`).
- **Group order.** Groups MUST be emitted in group-key order, matching
  the sort strategy's output order. Ordering K groups is O(K log K),
  not the O(n log n) sort of n rows this replaces.

Narrowings are permitted where the strategy structurally lacks something
the sort strategy has, and MUST fall back rather than approximate: no
explicit `GROUP BY` key (one group, nothing to save), and a `DISTINCT`
aggregate (whose dedup set is reopened per group boundary, which needs
adjacency).

See [ADR-0032](../../adr/0032-hash-group-by-second-strategy.md) for why
this is a new opcode family rather than a reuse of `OpenEphemeral`, why
groups are emitted in key order despite SQLite guaranteeing none, and
why group identity is a canonical key encoding rather than a `Hash`
instance on `Value`.

**Implementation:** `src/vdbe/hash_agg.rs`,
`src/codegen/select/aggregate/hash.rs::try_compile_hash_grouped_scan`

#### Scenario: A plain GROUP BY compiles the hash strategy, not the sorter

- GIVEN `SELECT bucket, count(*), sum(x) FROM t GROUP BY bucket` over a
  table with no index on `bucket`
- THEN the compiled program contains the full `HashAggOpen`/`HashAggFind`/
  `HashAggStep`/`HashAggRewind`/`HashAggData`/`HashAggNext` family, one
  `HashAggStep` per aggregate call, and no `SorterOpen`

**Tests:** `tests/unit/codegen_select_test.rs::plain_group_by_compiles_the_hash_aggregation_strategy`

#### Scenario: Several aggregates fold side by side into one hash table

- GIVEN `SELECT bucket, count(*), sum(x), avg(x), min(x), max(x) FROM t GROUP BY bucket`
- THEN every group's row matches the pinned oracle byte for byte, each
  aggregate having folded into its own accumulator slot

**Tests:** `tests/corpus/hash_group_by_test.rs::multiple_aggregates_in_one_query_match_oracle`

#### Scenario: A multi-column group key is encoded unambiguously

- GIVEN `GROUP BY a, b` over rows including `('a','bc')` and `('ab','c')`
- THEN they are distinct groups matching the oracle — a naive
  concatenation of key bytes would have merged them

**Tests:** `tests/corpus/hash_group_by_test.rs::multi_column_group_by_matches_oracle`,
`src/vdbe/hash_agg.rs::tests::multi_column_text_keys_are_unambiguous`

#### Scenario: NULL group keys form one group of their own

- GIVEN a `GROUP BY` column holding NULLs alongside other values
- THEN every NULL row lands in a single NULL group, never merged with
  any non-NULL value, matching the oracle

**Tests:** `tests/corpus/hash_group_by_test.rs::null_group_keys_match_oracle`,
`src/vdbe/hash_agg.rs::tests::null_and_missing_columns_share_the_null_key`

#### Scenario: A NOCASE-collated text key groups case-insensitively

- GIVEN a `TEXT COLLATE NOCASE` group-by column holding `'Ann'`, `'ann'`,
  `'ANN'`
- THEN they form one group, matching the oracle — the key is folded
  before hashing, not compared after

**Tests:** `tests/corpus/hash_group_by_test.rs::nocase_collated_text_group_keys_match_oracle`,
`src/vdbe/hash_agg.rs::tests::nocase_folds_text_keys_but_binary_does_not`

#### Scenario: INTEGER and REAL keys that compare equal share a group

- GIVEN group-by key values `1` and `1.0` (and `2` and `2.0`)
- THEN each pair forms one group, matching the oracle — and a REAL with
  a fractional part, or one outside `i64`'s range, never collides with
  an INTEGER

**Tests:** `tests/corpus/hash_group_by_test.rs::integer_and_real_group_keys_that_compare_equal_share_a_group`,
`src/vdbe/hash_agg.rs::tests::integer_and_exactly_equal_real_share_one_key`,
`src/vdbe/hash_agg.rs::tests::out_of_range_real_never_collides_with_an_integer`

#### Scenario: A key column's comparison affinity is applied before hashing

- GIVEN an `INTEGER`-declared group-by column holding both `1` and `'1'`
- THEN they form one group, matching the oracle — the same coercion the
  sort strategy's boundary `Eq` performs via its `P4::CollSeq` affinity
  byte

**Tests:** `tests/corpus/hash_group_by_test.rs::numeric_affinity_group_keys_match_oracle`,
`src/vdbe/hash_agg.rs::tests::numeric_affinity_groups_numeric_text_with_its_number`

#### Scenario: An explicit GROUP BY matching no rows produces no groups

- GIVEN an empty table, or a `WHERE` clause that excludes every row
- THEN zero rows are emitted — not the one all-NULL row an aggregate
  with no `GROUP BY` would produce

**Tests:** `tests/corpus/hash_group_by_test.rs::empty_result_set_matches_oracle`,
`src/vdbe/hash_agg.rs::tests::an_empty_table_jumps_past_the_loop`

#### Scenario: HAVING and LIMIT run unchanged at flush time

- GIVEN `GROUP BY ... HAVING count(*) > 18` and `... HAVING sum(x) > 1800 LIMIT 3`
- THEN the emitted rows match the oracle — the flush codegen is shared
  verbatim with the sort strategy, not reimplemented

**Tests:** `tests/corpus/hash_group_by_test.rs::group_by_with_having_and_limit_matches_oracle`

#### Scenario: A plain column takes the group's first row

- GIVEN a non-aggregate, non-grouped-by result column
- THEN it reads the group's first scanned row, the same "arbitrary row"
  the oracle's own sort-then-group strategy picks

**Tests:** `tests/corpus/hash_group_by_test.rs::plain_column_takes_the_same_arbitrary_row_as_the_oracle`,
`src/vdbe/hash_agg.rs::tests::the_first_row_of_a_group_is_the_one_retained`

#### Scenario: A DISTINCT aggregate falls back to the sort strategy

- GIVEN `SELECT bucket, count(DISTINCT x) FROM t GROUP BY bucket`
- THEN the compiled program contains `SorterOpen` and no `HashAggOpen`,
  and its rows still match the oracle — the fallback is exercised, not
  merely present

**Tests:** `tests/unit/codegen_select_test.rs::distinct_aggregate_group_by_still_compiles_the_sorter_strategy`,
`tests/corpus/hash_group_by_test.rs::distinct_aggregate_falls_back_to_the_sorter_and_still_matches_oracle`

#### Scenario: The shapes the index-ordered fast path declines are hashed

- GIVEN a `WHERE`-filtered `GROUP BY`, or a `GROUP BY` over a computed
  expression — neither of which
  `try_compile_index_ordered_group_by` accepts
- THEN both compile to the hash strategy (never the index walk) and
  match the oracle

**Tests:** `tests/corpus/hash_group_by_test.rs::where_filtered_and_computed_group_keys_match_oracle`,
`tests/corpus/index_ordered_group_by_test.rs::group_by_with_where_falls_back_to_hash_aggregation_and_still_matches_oracle`,
`tests/corpus/index_ordered_group_by_test.rs::group_by_over_expression_falls_back_to_hash_aggregation_and_still_matches_oracle`

## Traceability Note

Requirements 1, 2 (partial), 3, 4, 5 (partial), 6, 8, and 9 were made
active by #89 (VDBE core: instruction format, register file, control/
arithmetic/compare/result opcodes) and #90 (cursor, ephemeral-index, and
sorter opcode families). Requirements 7 (`Function` opcode dispatch), 10
(`EXPLAIN`), and 11 (expression emission) are now active too: #91 wired
the real SQL-to-`Program` pipeline (`src/codegen/`), the `Function`
opcode's dispatch (`src/vdbe/exec.rs`), and the `EXPLAIN` printer
(`src/vdbe/explain.rs`). Requirement 12 (`AggStep`/`AggFinal`) is now
active too: #241 added the two opcodes plus a minimal `count`/`sum`
aggregate registry (`src/vdbe/aggregate.rs`), reusing Requirement 4's
existing `OpenEphemeral` support as the grouping-table backing store;
#242 added the remaining `avg`/`min`/`max` aggregates to that registry.
#239/#242 initially shipped GROUP BY codegen via a separate hand-rolled
register-arithmetic scheme rather than these opcodes; #263 (ADR-0019)
rerouted that codegen onto `AggStep`/`AggFinal` and retired the
register-arithmetic scheme, fixing a collation gap and adding the `P5`
reset mechanism along the way.

`tests/unit/vdbe_opcode_completeness_test.rs` (#65) asserts `Opcode::ALL`
(`src/vdbe/program.rs`) exactly matches `tools/opcodes-v2.json`'s
harvested opcode set — the full 68-opcode inventory, independent of how
many are dispatched yet. `tools/assurance.py`'s `Opcode completeness:`
line tracks how many of those 68 are actually dispatched in
`src/vdbe/exec.rs` (currently 68/68 — every harvested opcode is
dispatched).
