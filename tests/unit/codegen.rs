//! Unit tests for expression lowering (spec 009 Requirement 11),
//! pinning the four codegen defects the sqllogictest slice surfaced
//! (#96, #131 review).
//!
//! These assert on the *emitted program shape* rather than on query
//! results, deliberately: this crate has no write path, so a
//! result-level test would need the pinned oracle to build fixture
//! state and would therefore skip whenever the oracle is absent. Shape
//! assertions need neither oracle nor fixture, so they run under plain
//! `make test` and actually gate a regression. Semantic coverage of the
//! same expressions lives in the oracle-backed parity/corpus suites.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use sqlite_rs::codegen::{compile_select, CodegenError};
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::schema::TableSchema;
use sqlite_rs::vdbe::{Opcode, Program};

/// `columns` must list exactly what `sql`'s column-definition list
/// declares — `column_index` resolves names against this vector while
/// `rowid_alias_column` re-derives its answer from `sql`, and the two
/// are only meaningful together.
fn schema(sql: &str, columns: &[&str]) -> TableSchema {
    TableSchema {
        name: "t".to_string(),
        root_page: 2,
        columns: columns.iter().map(|c| (*c).to_string()).collect(),
        column_types: vec![],
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
        indexes: vec![],
    }
}

fn compile(sql: &str, schema: &TableSchema) -> Program {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(select) => *select,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    match compile_select(&select, schema) {
        Ok(program) => program,
        Err(e) => panic!("expected {sql:?} to compile, got {e:?}"),
    }
}

fn compile_err(sql: &str, schema: &TableSchema) -> CodegenError {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(select) => *select,
        other => panic!("expected {sql:?} to parse, got {other:?}"),
    };
    match compile_select(&select, schema) {
        Ok(_) => panic!("expected {sql:?} to be rejected by codegen"),
        Err(e) => e,
    }
}

fn count(program: &Program, opcode: Opcode) -> usize {
    program
        .instructions
        .iter()
        .filter(|i| i.opcode == opcode)
        .count()
}

fn uses(program: &Program, opcode: Opcode) -> bool {
    count(program, opcode) > 0
}

// ---------------------------------------------------------------
// Rowid alias: an INTEGER PRIMARY KEY column is a NULL placeholder in
// every record, so reading it with `Column` yields NULL and
// `WHERE x = 2` silently matches nothing.
// ---------------------------------------------------------------

const IPK_DDL: &str = "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)";
const PLAIN_DDL: &str = "CREATE TABLE t (id INTEGER, name TEXT)";

#[test]
fn rowid_alias_result_column_reads_via_rowid_not_column() {
    let s = schema(IPK_DDL, &["id", "name"]);
    let program = compile("SELECT id FROM t", &s);
    assert!(
        uses(&program, Opcode::Rowid),
        "rowid-alias column must read through Rowid"
    );
    assert!(
        !uses(&program, Opcode::Column),
        "rowid-alias column must not be read through Column (it stores NULL)"
    );
}

#[test]
fn non_alias_column_still_reads_via_column() {
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT id FROM t", &s);
    assert!(uses(&program, Opcode::Column));
    assert!(
        !uses(&program, Opcode::Rowid),
        "an ordinary INTEGER column is stored normally and must not be substituted"
    );
}

#[test]
fn star_expansion_reads_the_rowid_alias_via_rowid() {
    // `SELECT id` was fixed with the other two call sites; `SELECT *`
    // was not, because the star-expansion path in `compile_row_values`
    // emits its own `Column` rather than going through
    // `emit_column_read`. `SELECT * FROM t` therefore answered NULL for
    // the rowid-alias column — the most common query in SQL, on the
    // most common table shape. No corpus fixture has an
    // `INTEGER PRIMARY KEY`, which is why the oracle suites never saw
    // it.
    let s = schema(IPK_DDL, &["id", "name"]);
    for sql in ["SELECT * FROM t", "SELECT t.* FROM t"] {
        let program = compile(sql, &s);
        assert!(
            uses(&program, Opcode::Rowid),
            "{sql:?} must read the rowid-alias column through Rowid"
        );
        assert_eq!(
            count(&program, Opcode::Column),
            1,
            "{sql:?} should read only the non-alias column through Column"
        );
    }
}

#[test]
fn rowid_alias_in_where_clause_reads_via_rowid() {
    // The bug's headline symptom: this returned no rows at all, because
    // the WHERE comparison read the placeholder NULL. Covers the
    // `emit_branch_into` call site rather than the result-column one.
    //
    // `id = 2` (an equality against the rowid alias) is deliberately not
    // used here: #137 pattern-matches exactly that shape into a
    // `SeekRowid` point lookup, which reads the row by seeking rather
    // than by comparing a `Rowid`-read register — see
    // `rowid_alias_equality_compiles_to_seek_rowid` below. `id > 2`
    // stays outside that fast path (range comparisons are out of scope
    // per #137) and still exercises the original bug's `Rowid`-read
    // fix.
    let s = schema(IPK_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id > 2", &s);
    assert!(
        uses(&program, Opcode::Rowid),
        "a rowid-alias column in WHERE must read through Rowid"
    );
}

#[test]
fn rowid_alias_equality_compiles_to_seek_rowid() {
    let s = schema(IPK_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id = 2", &s);
    assert!(
        uses(&program, Opcode::SeekRowid),
        "an equality on the rowid alias must compile to SeekRowid, not a full scan"
    );
    assert!(
        !uses(&program, Opcode::Rewind),
        "the SeekRowid fast path must not also emit the Rewind/Next scan loop"
    );
}

#[test]
fn bare_rowid_keyword_equality_compiles_to_seek_rowid() {
    let s = schema(IPK_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE rowid = 2", &s);
    assert!(uses(&program, Opcode::SeekRowid));
    assert!(!uses(&program, Opcode::Rewind));
}

#[test]
fn rowid_equality_against_parameter_compiles_to_seek_rowid() {
    let s = schema(IPK_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE rowid = ?", &s);
    assert!(uses(&program, Opcode::Variable));
    assert!(uses(&program, Opcode::SeekRowid));
    assert!(!uses(&program, Opcode::Rewind));
}

#[test]
fn rowid_range_comparison_does_not_use_seek_rowid() {
    let s = schema(IPK_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id > 2", &s);
    assert!(!uses(&program, Opcode::SeekRowid));
    assert!(uses(&program, Opcode::Rewind));
}

#[test]
fn non_rowid_column_equality_does_not_use_seek_rowid() {
    let s = schema(IPK_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE name = 'x'", &s);
    assert!(!uses(&program, Opcode::SeekRowid));
    assert!(uses(&program, Opcode::Rewind));
}

#[test]
fn without_rowid_table_never_substitutes_rowid() {
    let mut s = schema(IPK_DDL, &["id", "name"]);
    s.without_rowid = true;
    let program = compile("SELECT id FROM t", &s);
    assert!(uses(&program, Opcode::Column));
    assert!(!uses(&program, Opcode::Rowid));
}

// ---------------------------------------------------------------
// Aggregates without GROUP BY (#287): `count`/`sum`/`avg`/`min`/`max`
// now compile via the implicit whole-table group — previously (#268)
// every aggregate call without GROUP BY was rejected outright, since
// this codebase had no grouping pass at all yet. `total`/
// `group_concat` still have no `crate::vdbe::aggregate::AggState`
// accumulator (see `classify_aggregate`), so they remain rejected.
// ---------------------------------------------------------------

#[test]
fn aggregate_calls_without_group_by_compile_via_the_implicit_whole_table_group() {
    let s = schema(PLAIN_DDL, &["id", "name"]);
    for sql in [
        "SELECT count(*) FROM t",
        "SELECT count(id) FROM t",
        "SELECT sum(id) FROM t",
        "SELECT avg(id) FROM t",
        "SELECT max(id) FROM t",
        "SELECT min(id) FROM t",
    ] {
        drop(compile(sql, &s));
    }
}

#[test]
fn aggregates_without_an_agg_state_accumulator_are_still_rejected_as_unsupported() {
    let s = schema(PLAIN_DDL, &["id", "name"]);
    for sql in [
        "SELECT total(id) FROM t",
        "SELECT group_concat(name) FROM t",
    ] {
        match compile_err(sql, &s) {
            CodegenError::Unsupported { reason } => assert!(
                reason.contains("aggregate") || reason.contains("supported"),
                "{sql:?} rejected for the wrong reason: {reason}"
            ),
            other => panic!("{sql:?} expected Unsupported, got {other:?}"),
        }
    }
}

#[test]
fn scalar_functions_with_arguments_compile() {
    // Regression guard for a defect this suite surfaced: `Function`
    // reads its arguments from a contiguous register window, but codegen
    // reserved that window *before* compiling the args into freshly
    // allocated registers, so the contiguity check rejected every call
    // that had arguments. V2's scalar functions were unreachable through
    // the compiled query path entirely — `SELECT abs(id) FROM t`
    // included.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    for sql in [
        "SELECT abs(id) FROM t",
        "SELECT length(name) FROM t",
        "SELECT upper(name) FROM t",
        "SELECT abs(-1) FROM t",
        "SELECT substr(name, 1, 2) FROM t",
    ] {
        let program = compile(sql, &s);
        assert!(
            uses(&program, Opcode::Function),
            "{sql:?} should emit a Function call"
        );
    }
}

#[test]
fn multi_arg_max_min_are_scalars_and_still_compile() {
    // `max`/`min` are overloaded — the 2+-argument form is an ordinary
    // scalar function, so arity and not the name decides. Inverting this
    // carve-out on a refactor would silently reject valid SQL.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    for sql in ["SELECT max(id, 5) FROM t", "SELECT min(id, 5) FROM t"] {
        let program = compile(sql, &s);
        assert!(
            uses(&program, Opcode::Function),
            "{sql:?} should compile to a scalar Function call"
        );
    }
}

// ---------------------------------------------------------------
// Three-valued logic: both NOT forms were compiled as their positive
// form with true/false jump targets swapped, which turns SQL's
// "unknown" into "true" and returns rows for NULL operands.
// ---------------------------------------------------------------

#[test]
fn between_lowers_to_ge_and_le() {
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id BETWEEN 1 AND 10", &s);
    assert!(uses(&program, Opcode::Ge) && uses(&program, Opcode::Le));
}

#[test]
fn not_between_lowers_to_lt_or_gt_not_swapped_ge_le() {
    // SQLite lowers `x NOT BETWEEN lo AND hi` as `x < lo OR x > hi`.
    // A revert to "same shape, targets swapped" would show up here as
    // Ge/Le reappearing, which is exactly the NULL-becomes-true bug.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id NOT BETWEEN 1 AND 10", &s);
    assert!(
        uses(&program, Opcode::Lt) && uses(&program, Opcode::Gt),
        "NOT BETWEEN must lower to `x < lo OR x > hi`"
    );
    assert!(
        !uses(&program, Opcode::Ge) && !uses(&program, Opcode::Le),
        "NOT BETWEEN must not be the positive form with jump targets swapped"
    );
}

#[test]
fn in_list_emits_the_null_guard_machinery() {
    // `IN` can't collapse to a single true/false jump: it has three
    // outcomes (match / definite non-match / unknown-because-NULL). The
    // IsNull probes and the saw-NULL `IfNot` are that machinery; a
    // revert to the swapped-target form emits neither.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id IN (1, 2)", &s);
    assert!(
        count(&program, Opcode::IsNull) >= 3,
        "expected a NULL probe for the operand and for each list item"
    );
    assert!(uses(&program, Opcode::IfNot), "expected the saw-NULL guard");
}

#[test]
fn not_in_emits_the_same_null_guard_machinery_as_in() {
    // `NOT IN`'s definite-non-match and unknown outcomes diverge
    // (`NOT FALSE` is true, `NOT NULL` is still NULL), so it needs the
    // guard just as much as `IN` does — this is the case that returned
    // rows for NULL operands.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id NOT IN (1, 2)", &s);
    assert!(count(&program, Opcode::IsNull) >= 3);
    assert!(uses(&program, Opcode::IfNot), "expected the saw-NULL guard");
}

#[test]
fn empty_in_list_is_statically_false_without_null_probes() {
    // `x IN ()` is false even for NULL `x` — an empty list leaves
    // nothing to be uncertain against, so the guard machinery is
    // deliberately absent here.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id IN ()", &s);
    assert!(!uses(&program, Opcode::IsNull));
}

// ---------------------------------------------------------------
// Three-valued logic, generic `NOT` (#134). `compile_cond` now carries
// a `NullTarget` saying which continuation the *unknown* outcome joins;
// `NOT` swaps true/false and flips it, so NULL stays on the address it
// already had. The observable consequence in the emitted program is
// that a negated comparison gains explicit `IsNull` operand probes —
// the compare opcodes never jump on NULL, so routing unknown to the
// *true* side has to be spelled out. A revert to the bare target swap
// emits none of them.
// ---------------------------------------------------------------

/// Full instruction listing — opcode *and* operands, so a test that
/// compares two spellings of the same condition catches a divergence
/// in where a jump goes, not just in which opcodes were emitted.
fn listing(program: &Program) -> Vec<String> {
    program
        .instructions
        .iter()
        .map(|i| format!("{:?} {} {} {} {:?}", i.opcode, i.p1, i.p2, i.p3, i.p4))
        .collect()
}

#[test]
fn plain_comparison_needs_no_null_probe() {
    // Baseline for the two tests below: with unknown joining false —
    // which is what the compare opcodes already do by not jumping —
    // `WHERE x = 5` needs no probe at all.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id = 5", &s);
    assert!(!uses(&program, Opcode::IsNull));
}

#[test]
fn not_over_a_comparison_probes_for_null_instead_of_swapping_targets() {
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE NOT (id = 5)", &s);
    assert!(
        count(&program, Opcode::IsNull) >= 2,
        "NOT over a comparison must probe both operands for NULL so the \
         unknown outcome still excludes the row; a bare target swap \
         emits no probe and returns rows where id IS NULL"
    );
}

#[test]
fn ne_probes_for_null_like_a_negated_eq() {
    // `<>` has no opcode of its own — it is `Eq` with the targets
    // exchanged, the same shape as `NOT`, and it carried the same bug:
    // `WHERE id <> 5` returned rows where `id IS NULL`.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id <> 5", &s);
    assert!(count(&program, Opcode::IsNull) >= 2);
    assert_eq!(
        listing(&program),
        listing(&compile("SELECT name FROM t WHERE NOT (id = 5)", &s)),
        "`x <> 5` and `NOT (x = 5)` are the same condition and must compile alike"
    );
}

#[test]
fn not_in_and_in_negated_compile_to_the_same_program() {
    // The acceptance criterion #134 names: the two spellings are the
    // same condition, so they must not disagree. `NOT` routes unknown
    // to the swapped-in true target, which is the address `NOT IN`'s
    // own unknown path already used — making the two emissions
    // instruction-for-instruction identical, not merely equivalent.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    assert_eq!(
        listing(&compile("SELECT name FROM t WHERE NOT (id IN (1, 2))", &s)),
        listing(&compile("SELECT name FROM t WHERE id NOT IN (1, 2)", &s)),
    );
}

#[test]
fn not_in_value_context_uses_the_not_opcode() {
    // `SELECT NOT x` is one instruction in the oracle, and `Not` is the
    // only way a NULL survives negation into a register — the old
    // `IfNot`-based 0/1 materialization resolved NULL to 1.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT NOT id FROM t", &s);
    assert!(uses(&program, Opcode::Not));
    assert!(
        !uses(&program, Opcode::IfNot),
        "negation in value context must not go through a 0/1 truthiness test"
    );
}

#[test]
fn comparison_in_value_context_materializes_three_outcomes() {
    // `SELECT (x = 5)` answers 1, 0, or NULL. Before #134 it fell into
    // `compile_value`'s catch-all and answered NULL for every row.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT id = 5 FROM t", &s);
    assert!(
        uses(&program, Opcode::Null),
        "the unknown outcome needs a real NULL to land on"
    );
    assert!(uses(&program, Opcode::Eq) && uses(&program, Opcode::Integer));
}

#[test]
fn case_without_else_writes_null_rather_than_reading_a_phantom_column() {
    // The no-match path has to overwrite `dest`, which is shared across
    // branches and reused every scan iteration. It used to fake the
    // NULL with an out-of-range `Column` read for want of a `Null`
    // opcode.
    let s = schema(PLAIN_DDL, &["id", "name"]);
    let program = compile("SELECT CASE WHEN id > 5 THEN 'big' END FROM t", &s);
    assert!(uses(&program, Opcode::Null));
    let phantom = program
        .instructions
        .iter()
        .any(|i| i.opcode == Opcode::Column && i.p2 as usize >= s.columns.len());
    assert!(!phantom, "no out-of-range Column read should remain");
}
