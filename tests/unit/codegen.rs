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
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: sql.to_string(),
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
fn rowid_alias_in_where_clause_reads_via_rowid() {
    // The bug's headline symptom: this returned no rows at all, because
    // the WHERE comparison read the placeholder NULL. Covers the
    // `emit_branch_into` call site rather than the result-column one.
    let s = schema(IPK_DDL, &["id", "name"]);
    let program = compile("SELECT name FROM t WHERE id = 2", &s);
    assert!(
        uses(&program, Opcode::Rowid),
        "a rowid-alias column in WHERE must read through Rowid"
    );
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
// Aggregates: compiled as ordinary per-row scalar calls, so
// `SELECT count(*) FROM t` emitted one row per input row. V2 has no
// grouping pass, so codegen refuses rather than emitting wrong output.
// ---------------------------------------------------------------

#[test]
fn aggregate_calls_are_rejected_as_unsupported() {
    let s = schema(PLAIN_DDL, &["id", "name"]);
    for sql in [
        "SELECT count(*) FROM t",
        "SELECT count(id) FROM t",
        "SELECT sum(id) FROM t",
        "SELECT avg(id) FROM t",
        "SELECT total(id) FROM t",
        "SELECT group_concat(name) FROM t",
        "SELECT max(id) FROM t",
        "SELECT min(id) FROM t",
    ] {
        match compile_err(sql, &s) {
            CodegenError::Unsupported { reason } => assert!(
                reason.contains("aggregate"),
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
