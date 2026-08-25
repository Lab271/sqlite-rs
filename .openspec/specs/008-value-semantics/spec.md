---
domain: semantics
version: 0.1.0
status: draft
date: 2026-08-15
---

# 008 — Value Semantics

The value-semantics kernel — SQLite's `vdbemem.c`/`func.c` behavior as
requirements, before any VDBE code exists. Backs V2 phase 2 (#77), part of
epic #56. Every scenario is backed by a vector harvested from the pinned
oracle (`tools/gen_expr_vectors.py`, committed under
`tests/corpus/expr_vectors/`); spike 008 (#59) later ratchets (adds to)
these same files once a throwaway tree-walker exists to exercise them.
Refs: 001/Req-3.

## Philosophy

SQLite's type system is dynamic (per-value, not per-column) but column
declarations still steer storage through *affinity* — a five-way
preference, not an enforced type. Getting this wrong is SQLite's classic
divergence trap: naive readers assume static columns typed by their
declaration, and get silently wrong comparisons and sort orders as a
result. As with spec 004, we do not define correctness — the pinned
oracle does. Every rule here traces to a `quote()`/`typeof()` probe
against sqlite3 3.53.3, not to a paraphrase of the docs.

Grammar is untouched by this spec — it is semantics, not syntax (see
`.openspec/grammar/sqlite.ebnf`).

## Requirements

### Requirement 1: Type Affinity [MUST]

Every table column MUST be assigned one of five affinities (TEXT,
NUMERIC, INTEGER, REAL, BLOB) from its declared type per the substring
rules in [datatype3.html](https://www.sqlite.org/datatype3.html) §3.1: a
declared type containing "INT" gets INTEGER affinity; containing "CHAR",
"CLOB", or "TEXT" gets TEXT; containing "BLOB" or no declared type gets
BLOB; containing "REAL", "FLOA", or "DOUB" gets REAL; anything else gets
NUMERIC. A value MUST be converted to its column's affinity on storage
where a lossless conversion exists (NUMERIC/INTEGER/REAL affinities
coerce a well-formed numeric-text literal; BLOB and TEXT affinities never
convert).

**Implementation:** `src/vdbe/affinity.rs`

**Corpus:** `tests/corpus/expr_vectors/affinity.jsonl`

#### Scenario: INTEGER-family declared types share INTEGER affinity

- GIVEN columns declared INTEGER, INT, TINYINT, SMALLINT, MEDIUMINT,
  BIGINT, "UNSIGNED BIG INT", INT2, or INT8
- WHEN a text literal `'1.5'` is inserted
- THEN it is stored as a REAL (SQLite coerces to the best lossless
  numeric representation for non-TEXT/BLOB affinities; `1.5` has no
  lossless integer form)

**Tests:** `tests/corpus/expr_vectors_test.rs::affinity_vectors_include_declared_type_rules_table_entries`

#### Scenario: TEXT-family declared types share TEXT affinity and never convert

- GIVEN columns declared TEXT, CHARACTER(20), VARCHAR(255), "VARYING
  CHARACTER(255)", NCHAR(55), "NATIVE CHARACTER(70)", NVARCHAR(100), or
  CLOB
- WHEN a text literal `'1.5'` is inserted
- THEN it is stored unchanged as TEXT (no numeric coercion under TEXT
  affinity)

**Tests:** `tests/corpus/expr_vectors_test.rs::affinity_vectors_cover_all_five_affinity_classes`

#### Scenario: BLOB affinity applies when declared BLOB or undeclared

- GIVEN a column declared BLOB, or a column with no declared type at all
- WHEN a text literal `'1.5'` is inserted
- THEN it is stored unchanged as TEXT (BLOB affinity performs no
  conversion; the storage class is whatever the literal's own class is)

**Tests:** `tests/corpus/expr_vectors_test.rs::affinity_vectors_include_declared_type_rules_table_entries`

#### Scenario: Everything else defaults to NUMERIC affinity

- GIVEN columns declared REAL, DOUBLE, "DOUBLE PRECISION", FLOAT, NUMERIC,
  DECIMAL(10,5), BOOLEAN, DATE, DATETIME, or an unrecognized declared type
  (e.g. POINT, STRING) that matches none of the INTEGER/TEXT/BLOB
  substring rules
- WHEN a text literal `'1.5'` is inserted
- THEN it is stored as a REAL (REAL-declared columns get NUMERIC-class
  behavior identically to genuinely unrecognized declarations — REAL is
  not its own storage-affinity distinction from NUMERIC for this
  conversion)

**Tests:** `tests/corpus/expr_vectors_test.rs::affinity_vectors_cover_all_five_affinity_classes`

### Requirement 2: Cross-Type Comparison Order [MUST]

Values of different storage classes MUST compare in the fixed order
NULL < numeric < text < blob, regardless of collation. The numeric class
merges INTEGER and REAL — a value's magnitude is compared numerically
across both, not by storage-class tiebreak. A comparison against NULL
MUST itself yield NULL (see Requirement 4).

**Implementation:** `src/vdbe/compare.rs`

**Corpus:** `tests/corpus/expr_vectors/comparison.jsonl`

#### Scenario: NULL is lower than every other class

- GIVEN `NULL < 1`, `NULL < 'a'`, and `NULL < x'00'`
- THEN each evaluates to NULL, not a boolean (comparison against NULL is
  never true or false — see Requirement 4)

**Tests:** `tests/corpus/expr_vectors_test.rs::comparison_vectors_cover_null_numeric_text_blob_ordering`

#### Scenario: Numeric sorts below text, text sorts below blob

- GIVEN `1 < 'a'`, `1 < x'00'`, and `'a' < x'00'`
- THEN all three evaluate to 1 (true) — class order dominates value,
  so any number is less than any text and any text is less than any blob

**Tests:** `tests/corpus/expr_vectors_test.rs::comparison_vectors_cover_null_numeric_text_blob_ordering`

#### Scenario: INTEGER and REAL merge into one numeric class

- GIVEN `2 = 2.0` and `9223372036854775807 < 9223372036854775807.0`
- THEN `2 = 2.0` evaluates to 1 (equal by magnitude across storage
  classes) and the boundary comparison evaluates to 1, reflecting REAL's
  precision loss at `i64::MAX` (`9223372036854775807.0` rounds up past
  the exact integer value)

**Tests:** `tests/corpus/expr_vectors_test.rs::comparison_vectors_cover_null_numeric_text_blob_ordering`

### Requirement 3: Collations [MUST]

Text comparisons MUST use the applicable collating function: BINARY
(byte-for-byte, the default), NOCASE (ASCII-only case folding — NOT
Unicode; `ß`/`SS` and `é`/`É` do NOT compare equal), or RTRIM (BINARY
comparison after stripping trailing spaces from both operands, not from
storage).

**Implementation:** `src/record/collation.rs`; a column/index-column's
declared `COLLATE` (parsed but previously unstored, #500) is carried on
`TableSchema::column_collations` / `IndexedColumn::collation`
(`src/schema/ddl_reader.rs`) and consulted by every comparison site that
doesn't spell out an explicit `COLLATE` in the query text — an explicit
`COLLATE` always wins (`src/codegen/expr/value.rs::expr_collation`).

**Corpus:** `tests/corpus/expr_vectors/collation.jsonl`

#### Scenario: BINARY is case-sensitive byte comparison

- GIVEN `'abc' = 'ABC' COLLATE BINARY`
- THEN it evaluates to 0 (false)

**Tests:** `tests/corpus/expr_vectors_test.rs::collation_vectors_cover_binary_nocase_rtrim`

#### Scenario: NOCASE folds ASCII case only

- GIVEN `'I' = 'i' COLLATE NOCASE` and `'straße' = 'STRASSE' COLLATE NOCASE`
- THEN the ASCII pair evaluates to 1 (true) but the German pair evaluates
  to 0 (false) — NOCASE has no notion of `ß`/`SS` equivalence, a common
  divergence trap for implementers reaching for a locale-aware fold

**Tests:** `tests/corpus/expr_vectors_test.rs::collation_vectors_cover_binary_nocase_rtrim`

#### Scenario: NOCASE does not fold non-ASCII accented letters

- GIVEN `'é' = 'É' COLLATE NOCASE`
- THEN it evaluates to 0 (false) — NOCASE's fold table covers only
  ASCII A-Z/a-z

**Tests:** `tests/corpus/expr_vectors_test.rs::collation_vectors_cover_binary_nocase_rtrim`

#### Scenario: RTRIM ignores only trailing spaces

- GIVEN `'abc ' = 'abc' COLLATE RTRIM` and `'abc' = 'abc  ' COLLATE RTRIM`
- THEN both evaluate to 1 (true) regardless of which side carries the
  trailing spaces or how many

**Tests:** `tests/corpus/expr_vectors_test.rs::collation_vectors_cover_binary_nocase_rtrim`

#### Scenario: A column's declared COLLATE applies without an explicit query-side COLLATE

- GIVEN `CREATE TABLE t(name TEXT COLLATE NOCASE)` and `WHERE name = 'x'`
  (no `COLLATE` written in the query)
- THEN the comparison, `ORDER BY`/`GROUP BY` key comparisons, and index
  seeks/duplicate-key rechecks against `name` all use NOCASE, matching
  real `sqlite3` — an explicit query-side `COLLATE` still overrides it

**Tests:** `tests/corpus/declared_collate_test.rs::table_schema_captures_declared_column_collation`, `tests/corpus/declared_collate_test.rs::index_schema_captures_declared_column_collation`, `tests/corpus/declared_collate_test.rs::covering_index_seek_uses_declared_collation_without_explicit_collate`, `tests/corpus/declared_collate_test.rs::declared_collation_matches_every_case_varying_duplicate`, `tests/corpus/declared_collate_test.rs::covering_index_seek_and_recheck_compile_with_declared_collation_p4`, `tests/corpus/declared_collate_test.rs::order_by_uses_declared_collation_without_explicit_collate`, `tests/corpus/declared_collate_test.rs::group_by_uses_declared_collation_without_explicit_collate`

### Requirement 4: NULL Semantics [MUST]

NULL MUST propagate through arithmetic, concatenation, and comparison
operators (any operator with a NULL operand evaluates to NULL), except
`IS`/`IS NOT`, which MUST treat NULL as a comparable value rather than
propagating it. Boolean operators MUST follow three-valued logic: `NULL
AND 0` is 0 (false dominates), `NULL AND 1` is NULL; `NULL OR 1` is 1
(true dominates), `NULL OR 0` is NULL. `NOT NULL` is NULL.

**Implementation:** `src/vdbe/value.rs`

**Corpus:** `tests/corpus/expr_vectors/null.jsonl`

#### Scenario: NULL propagates through arithmetic, concatenation, and equality

- GIVEN `NULL + 1`, `NULL || 'x'`, `NULL = NULL`, and `1 = NULL`
- THEN all four evaluate to NULL, never to a boolean or numeric value

**Tests:** `tests/corpus/expr_vectors_test.rs::null_vectors_cover_three_valued_logic_and_is_vs_eq`

#### Scenario: IS and IS NOT treat NULL as an ordinary comparable value

- GIVEN `NULL IS NULL`, `NULL IS NOT NULL`, and `NULL IS 1`
- THEN they evaluate to 1, 0, and 0 respectively — never NULL, unlike `=`

**Tests:** `tests/corpus/expr_vectors_test.rs::null_vectors_cover_three_valued_logic_and_is_vs_eq`

#### Scenario: AND/OR follow three-valued logic

- GIVEN `NULL AND 0`, `NULL AND 1`, `NULL OR 1`, and `NULL OR 0`
- THEN they evaluate to 0, NULL, 1, and NULL respectively — a dominant
  false short-circuits AND to false even with a NULL operand, and a
  dominant true short-circuits OR to true, but otherwise NULL propagates

**Tests:** `tests/corpus/expr_vectors_test.rs::null_vectors_cover_three_valued_logic_and_is_vs_eq`

### Requirement 5: Numeric Coercion [MUST]

Text-to-numeric coercion MUST parse the longest valid numeric prefix of
the string (a leading run of optional sign, digits, decimal point, and
exponent), treating a non-numeric or empty string as `0`. Integer
arithmetic that overflows `i64` MUST promote the result to REAL rather
than silently wrapping (the CVE-2025-29087/3277 class). `CAST(... AS
INTEGER)` on a REAL truncates toward zero.

**Implementation:** `src/vdbe/coerce.rs`

**Corpus:** `tests/corpus/expr_vectors/coercion.jsonl`

#### Scenario: Text-to-numeric coercion parses the longest valid numeric prefix

- GIVEN `'123abc' + 1`, `'  123  ' + 1`, and `'abc' + 1`
- THEN they evaluate to 124, 124, and 1 respectively — leading/trailing
  whitespace is tolerated, a non-numeric suffix is discarded, and a
  wholly non-numeric string coerces to 0

**Tests:** `tests/corpus/expr_vectors_test.rs::coercion_vectors_cover_text_parsing_and_overflow_promotion`

#### Scenario: Integer overflow promotes to REAL, never wraps

- GIVEN `9223372036854775807 + 1` (one past `i64::MAX`) and
  `9223372036854775807 * 2`
- THEN both evaluate to a REAL result approximating the true magnitude,
  never to a wrapped or truncated `i64` — checked arithmetic, no silent
  overflow

**Tests:** `tests/corpus/expr_vectors_test.rs::coercion_vectors_cover_text_parsing_and_overflow_promotion`

#### Scenario: CAST to INTEGER truncates toward zero

- GIVEN `cast(3.9 AS INTEGER)` and `cast(-3.9 AS INTEGER)`
- THEN they evaluate to 3 and -3 respectively — truncation, not rounding
  or floor

**Tests:** `tests/corpus/expr_vectors_test.rs::coercion_vectors_cover_text_parsing_and_overflow_promotion`

### Requirement 6: Scalar Function Core [MUST]

The V2 scalar set (`length`, `upper`, `lower`, `substr`, `abs`, `coalesce`,
`ifnull`, `nullif`, `typeof`, `hex`, `unhex`, `quote`, scalar `min`/`max`,
`round`, `sign`, `instr`, `trim`/`ltrim`/`rtrim`, `replace`, `zeroblob`,
`iif`, `like`, `glob`) MUST be implemented as pure `fn(&[Value]) -> Result<Value,
FunctionError>` and dispatched through a case-insensitive name+arity
registry. Most functions MUST propagate NULL on any NULL argument;
`coalesce`/`ifnull` are the documented exception (first non-NULL
argument, or NULL if all are NULL). `upper`/`lower` MUST fold ASCII case
only, matching NOCASE (Requirement 3) — never Unicode case folding.
`length()` MUST count UTF-8 characters for TEXT, bytes for BLOB, and the
character length of the CAST-to-TEXT representation for numeric
arguments. `substr()`'s index arithmetic (negative/zero `Y`, negative
`Z`) MUST match SQLite's `substrFunc` exactly, not a simplified
one-sided-negative-index approximation.

**Implementation:** `src/vdbe/functions.rs`

**Corpus:** `tests/corpus/expr_vectors/functions.jsonl`

**Known gap:** `quote()`'s REAL rendering reuses [`format_real`]'s
15-significant-digit rule rather than SQLite's own higher-precision
`quote()` routine (observed up to ~19 significant digits on irrational
sums, and itself build-dependent across sqlite3 binaries — see the
`src/format.rs` REAL-rendering note for the identical divergence already
scoped out of `.dump`/`-list`, issue #37). Exact-precision `quote()` on
REAL is tracked as a follow-up, not solved by this requirement.

#### Scenario: Most functions propagate NULL; coalesce/ifnull are the exception

- GIVEN `length(NULL)` and `coalesce(NULL, NULL, 3)`
- THEN `length(NULL)` evaluates to NULL, but `coalesce(NULL, NULL, 3)`
  evaluates to `3` — coalesce/ifnull skip NULL arguments rather than
  propagating

**Tests:** `tests/corpus/expr_vectors_test.rs::function_vectors_cover_null_propagation_and_the_coalesce_exception`

#### Scenario: upper/lower fold ASCII case only

- GIVEN `upper('café')`
- THEN it evaluates to `'CAFé'`, not `'CAFÉ'` — the `é` is left
  unmodified because ASCII-only folding has no notion of non-ASCII case

**Tests:** `tests/corpus/expr_vectors_test.rs::function_vectors_cover_ascii_only_case_folding`

#### Scenario: substr handles negative and zero indices per substrFunc

- GIVEN `substr('hello', -3)`, `substr('hello', 0)`,
  `substr('hello', 2, -1)`, and `substr('hello', -100, 2)`
- THEN they evaluate to `'llo'`, `'hello'`, `'h'`, and `''` respectively —
  negative `Y` counts from the end, `Y = 0` behaves like `Y = 1` for the
  no-length form, negative `Z` takes the `abs(Z)` characters preceding
  position `Y`, and an out-of-range negative `Y` clamps rather than
  panicking or wrapping

**Tests:** `tests/corpus/expr_vectors_test.rs::function_vectors_cover_substr_negative_and_zero_index_rules`

#### Scenario: quote() escapes embedded single quotes byte-exact

- GIVEN `quote('it''s')`
- THEN it evaluates to `'''it''''s'''` — every single quote in the input,
  including ones already escaped in the SQL literal, is doubled in the
  output

**Tests:** `tests/corpus/expr_vectors_test.rs::function_vectors_quote_output_is_byte_exact_with_escaped_quotes`

#### Scenario: like/glob are registry functions, not a separate matcher

- GIVEN `like('abc','ABC')` and `glob('abc','ABC')` (note SQLite's
  reversed argument order: pattern first, then text)
- THEN `like` matches ASCII case-insensitively (`1`) while `glob` is
  case-sensitive (`0`), both dispatched through the same name+arity
  registry as every other scalar function — so spec 009's `Function`
  opcode (Requirement 7 there, which names `"like(2)"` as a P4
  descriptor) needs no LIKE-specific VDBE logic

**Tests:** `src/vdbe/functions.rs::tests::like_and_glob_match_oracle_semantics`
