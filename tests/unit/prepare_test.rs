// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! `codegen::prepare` — the `SELECT` compile pipeline, as a library
//! function rather than a CLI internal (#695).
//!
//! These exist because the lift made `compile_select_program` and
//! `result_column_names` public API with no direct coverage: everything
//! that exercised them went through the `sqlite-rs` binary, so a
//! regression in the library surface would only have shown up as a CLI
//! failure. The shapes below are the four `compile_select_program`
//! dispatches on (FROM-less, single-table, joined, compound) plus the
//! one error that is about the request rather than the SQL.
//!
//! Row-level agreement with the oracle is
//! `tests/corpus/prepare_oracle_test.rs`'s job; this file is about the
//! dispatch and the error type.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;

use sqlite_rs::codegen::{
    compile_select_program, result_column_names, PrepareError, SelectOutcome,
};
use sqlite_rs::parser::error::ParseOutcome;
use sqlite_rs::parser::{parse_explain, parse_select};
use sqlite_rs::planner::Stats;
use sqlite_rs::record::Collation;
use sqlite_rs::schema::{TableSchema, ViewSchema};

/// Two tables, declared the way `ddl_reader` would report them.
fn catalog() -> Vec<TableSchema> {
    vec![table("t", &["a", "b"]), table("u", &["a", "c"])]
}

fn table(name: &str, columns: &[&str]) -> TableSchema {
    TableSchema {
        name: name.to_string(),
        root_page: 2,
        columns: columns.iter().map(|c| (*c).to_string()).collect(),
        column_types: columns.iter().map(|_| "INTEGER".to_string()).collect(),
        column_collations: columns.iter().map(|_| Collation::Binary).collect(),
        without_rowid: false,
        strict: false,
        is_virtual: false,
        sql: format!("CREATE TABLE {name}({})", columns.join(", ")),
        indexes: vec![],
        rowid_alias: None,
        unresolved_autoindex: false,
    }
}

fn select_of(sql: &str) -> sqlite_rs::parser::ast::Select {
    match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("{sql} did not parse: {other:?}"),
    }
}

fn compile(sql: &str) -> Result<SelectOutcome, PrepareError> {
    let select = select_of(sql);
    compile_select_program(&select, false, &catalog(), &[], &HashMap::new())
}

fn program_of(sql: &str) -> sqlite_rs::vdbe::Program {
    match compile(sql).unwrap_or_else(|e| panic!("{sql} did not compile: {e}")) {
        SelectOutcome::Program(p) => p,
        SelectOutcome::Eqp(_) => panic!("{sql} produced EQP rows, not a program"),
    }
}

#[test]
fn compiles_all_four_select_shapes() {
    // The dispatch inside `compile_select_program`, one case each. A
    // non-empty program is the assertion: each arm reaches a different
    // compiler, and a silent fallthrough would come back empty or error.
    for sql in [
        "SELECT 1 + 1",                               // FROM-less (#260)
        "SELECT a, b FROM t WHERE a > 1",             // single-table
        "SELECT t.a, u.c FROM t JOIN u ON t.a = u.a", // joined (#237)
        "SELECT a FROM t UNION ALL SELECT a FROM u",  // compound (#240)
    ] {
        let program = program_of(sql);
        assert!(
            !program.instructions.is_empty(),
            "{sql} compiled to an empty program"
        );
    }
}

#[test]
fn explain_query_plan_returns_rows_not_a_program() {
    let sql = "EXPLAIN QUERY PLAN SELECT a FROM t WHERE a > 1";
    let select = match parse_explain(sql) {
        ParseOutcome::Accepted(e) => e,
        other => panic!("{sql} did not parse: {other:?}"),
    };
    let outcome =
        compile_select_program(&select.select, true, &catalog(), &[], &HashMap::new()).unwrap();
    match outcome {
        SelectOutcome::Eqp(rows) => assert!(!rows.is_empty(), "EQP produced no rows"),
        SelectOutcome::Program(_) => panic!("eqp_mode returned a program"),
    }
}

/// The one error that is about the *request* rather than the SQL, and
/// the reason `PrepareError` exists rather than reusing `CodegenError`:
/// `SELECT 1` compiles perfectly well, it just has no access path to
/// explain.
#[test]
fn eqp_without_from_is_its_own_error() {
    let select = select_of("SELECT 1");
    let err = compile_select_program(&select, true, &catalog(), &[], &HashMap::new())
        .expect_err("EQP on a FROM-less SELECT should fail");

    assert_eq!(err, PrepareError::EqpWithoutFrom);
    // Byte-identical to the string the CLI printed before the lift.
    assert_eq!(err.to_string(), "EXPLAIN QUERY PLAN requires a FROM clause");

    // And the same statement without `eqp_mode` still compiles, which is
    // what makes this distinct from `CodegenError::NoFromClause`.
    assert!(matches!(
        compile_select_program(&select, false, &catalog(), &[], &HashMap::new()),
        Ok(SelectOutcome::Program(_))
    ));
}

#[test]
fn a_missing_table_surfaces_as_a_codegen_error_not_a_string() {
    let select = select_of("SELECT a FROM nonexistent");
    let err = compile_select_program(&select, false, &catalog(), &[], &HashMap::new())
        .expect_err("unknown table should fail");

    // The point of the lift's error change: a caller can match on the
    // variant instead of parsing a message.
    assert!(
        matches!(err, PrepareError::Codegen(_)),
        "expected a wrapped CodegenError, got {err:?}"
    );
    assert!(
        err.to_string().contains("nonexistent"),
        "message should name the table: {err}"
    );
}

#[test]
fn result_column_names_uses_real_names_for_a_single_table() {
    let select = select_of("SELECT a, b FROM t");
    assert_eq!(result_column_names(&select, &catalog()), vec!["a", "b"]);
}

#[test]
fn result_column_names_honours_aliases() {
    // Non-keyword aliases on purpose. `AS first` is what this test used
    // first, and it failed — not because aliasing is broken but because
    // `FIRST` is one of the 89 keywords we reserve that SQLite treats as
    // an identifier (#696). That is a parser bug, not a `prepare` bug,
    // so it is pinned there rather than smuggled in here.
    let select = select_of("SELECT a AS alpha, b AS beta FROM t");
    assert_eq!(
        result_column_names(&select, &catalog()),
        vec!["alpha", "beta"]
    );
}

/// Joins and compounds fall back to positional names. Asserted rather
/// than left implicit because `Row::get_by_name` will be built on this,
/// and "column1" is a real answer a consumer can receive — not a bug.
#[test]
fn result_column_names_falls_back_positionally_for_joins_and_compounds() {
    let joined = select_of("SELECT t.a, u.c FROM t JOIN u ON t.a = u.a");
    assert_eq!(
        result_column_names(&joined, &catalog()),
        vec!["column1", "column2"]
    );

    let compound = select_of("SELECT a FROM t UNION ALL SELECT a FROM u");
    assert_eq!(result_column_names(&compound, &catalog()), vec!["column1"]);
}

/// An unknown table makes name derivation fall back rather than panic —
/// `result_column_names` returns names, not a `Result`, so it has to
/// have an answer for every input.
#[test]
fn result_column_names_falls_back_when_the_table_is_unknown() {
    let select = select_of("SELECT a, b FROM nonexistent");
    assert_eq!(
        result_column_names(&select, &catalog()),
        vec!["column1", "column2"]
    );
}

/// `views` is threaded through the pipeline; an empty catalog must not
/// make a plain `SELECT` fail.
#[test]
fn an_empty_view_catalog_is_fine() {
    let select = select_of("SELECT a FROM t");
    let views: Vec<ViewSchema> = vec![];
    assert!(matches!(
        compile_select_program(&select, false, &catalog(), &views, &HashMap::new()),
        Ok(SelectOutcome::Program(_))
    ));
}

/// Stats are consulted for the single-table path; a table with no
/// `sqlite_stat1` row must compile the same as one with stats present.
#[test]
fn missing_stats_compile_the_same_as_present_stats() {
    let select = select_of("SELECT a FROM t WHERE a > 1");
    let without = program_of("SELECT a FROM t WHERE a > 1");

    let mut stats = HashMap::new();
    stats.insert("t".to_string(), Stats::default());
    let with = match compile_select_program(&select, false, &catalog(), &[], &stats).unwrap() {
        SelectOutcome::Program(p) => p,
        SelectOutcome::Eqp(_) => panic!("unexpected EQP"),
    };

    assert_eq!(
        without.instructions.len(),
        with.instructions.len(),
        "default stats should not change the plan"
    );
}
