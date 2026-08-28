// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Result-level correctness for #606's index range-seek fast paths
//! (`BETWEEN`/`IN`/`LIKE`/`GLOB` against an indexed column) — mirrors
//! `tests/unit/codegen_select_test.rs`'s scratch-db-plus-`our_rows`
//! pattern (no pinned-oracle dependency: expected rows are hardcoded,
//! not cross-checked against a second engine), so this runs under
//! plain `make test`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::{compile_select, explain_query_plan};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::parser::{parse_select, ParseOutcome};
use sqlite_rs::record::Value;
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::{execute_with_db, Opcode, Program};
use sqlite_rs::vfs::{UnixVfs, Vfs, VfsPageSource};

fn scratch_db(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "sqlite_rs_range_scan_test_{}_{}.db",
        std::process::id(),
        label
    ));
    std::fs::remove_file(&path).ok();
    path
}

fn seed(db: &Path, sql: &str) {
    let status = Command::new("sqlite3").arg(db).arg(sql).status().unwrap();
    assert!(status.success());
}

fn table_schema(db: &Path, table: &str) -> TableSchema {
    let vfs = UnixVfs;
    let file = vfs.open_read(db).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source = VfsPageSource::open(&vfs, db, header.page_size).unwrap();
    let mut cursor = TableCursor::new(source, &header, 1);
    let schemas = read_schema(&mut cursor, header.text_encoding).unwrap();
    schemas
        .into_iter()
        .find(|s| s.name == table)
        .unwrap_or_else(|| panic!("no schema for table {table}"))
}

fn compile(schema: &TableSchema, sql: &str) -> Program {
    let select = match parse_select(sql) {
        ParseOutcome::Accepted(s) => *s,
        other => panic!("expected accept for {sql:?}, got {other:?}"),
    };
    compile_select(&select, schema).unwrap_or_else(|e| panic!("compiling {sql:?}: {e:?}"))
}

fn run_rows(db: &Path, schema: &TableSchema, sql: &str) -> Vec<Vec<Value>> {
    let program = compile(schema, sql);
    let vfs = UnixVfs;
    let file = vfs.open_read(db).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source = VfsPageSource::open(&vfs, db, header.page_size).unwrap();
    execute_with_db(&program, Rc::new(source), header).unwrap()
}

fn uses(program: &Program, opcode: Opcode) -> bool {
    program.instructions.iter().any(|i| i.opcode == opcode)
}

fn int_fixture(label: &str) -> (PathBuf, TableSchema) {
    let db = scratch_db(label);
    seed(
        &db,
        "CREATE TABLE t(id INTEGER, val INTEGER); \
         CREATE INDEX idx_val ON t(val); \
         INSERT INTO t VALUES (1, 5), (2, 10), (3, 15), (4, 20), (5, 25);",
    );
    let schema = table_schema(&db, "t");
    (db, schema)
}

fn text_fixture(label: &str) -> (PathBuf, TableSchema) {
    let db = scratch_db(label);
    seed(
        &db,
        "CREATE TABLE t(id INTEGER, name TEXT); \
         CREATE INDEX idx_name ON t(name); \
         INSERT INTO t VALUES \
            (1, 'foo'), (2, 'foobar'), (3, 'foP'), (4, 'far'), (5, 'fop'), (6, 'zzz');",
    );
    let schema = table_schema(&db, "t");
    (db, schema)
}

// ---------------------------------------------------------------
// BETWEEN
// ---------------------------------------------------------------

#[test]
fn between_includes_both_boundaries() {
    let (db, schema) = int_fixture("between_boundaries");
    let program = compile(&schema, "SELECT id FROM t WHERE val BETWEEN 10 AND 20");
    assert!(uses(&program, Opcode::SeekIndexGE));

    let rows = run_rows(&db, &schema, "SELECT id FROM t WHERE val BETWEEN 10 AND 20");
    let ids: Vec<i64> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Integer(i) => *i,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![2, 3, 4]);
}

/// #606 regression: a string-literal operand against an
/// `INTEGER`-affinity indexed column must NOT take the range-seek fast
/// path — the seek would build a raw `Text` probe key compared
/// byte-for-byte against the index's actual `Integer`-affinity-coerced
/// entries, silently missing every row (SQLite's own comparison
/// affinity coercion, applied dynamically by `Ge`/`Le` in the ordinary
/// filter path, is not reproduced by a seek). Falls back to the
/// ordinary scan, which gets this right.
#[test]
fn between_falls_back_for_affinity_mismatched_string_operand() {
    let (db, schema) = int_fixture("between_affinity_mismatch");
    let program = compile(&schema, "SELECT id FROM t WHERE val BETWEEN '10' AND '20'");
    assert!(!uses(&program, Opcode::SeekIndexGE));

    let rows = run_rows(
        &db,
        &schema,
        "SELECT id FROM t WHERE val BETWEEN '10' AND '20'",
    );
    let ids: Vec<i64> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Integer(i) => *i,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![2, 3, 4]);
}

/// Same hazard as above, for `IN`.
#[test]
fn in_list_falls_back_for_affinity_mismatched_string_operand() {
    let (db, schema) = int_fixture("in_affinity_mismatch");
    let program = compile(&schema, "SELECT id FROM t WHERE val IN ('10', '20')");
    assert!(!uses(&program, Opcode::SeekIndexEq));

    let rows = run_rows(&db, &schema, "SELECT id FROM t WHERE val IN ('10', '20')");
    let ids: Vec<i64> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Integer(i) => *i,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, vec![2, 4]);
}

#[test]
fn between_with_no_matching_rows_is_empty() {
    let (db, schema) = int_fixture("between_no_match");
    let rows = run_rows(
        &db,
        &schema,
        "SELECT id FROM t WHERE val BETWEEN 100 AND 200",
    );
    assert!(rows.is_empty());
}

// ---------------------------------------------------------------
// IN
// ---------------------------------------------------------------

#[test]
fn in_list_matches_exactly_the_listed_values() {
    let (db, schema) = int_fixture("in_list_values");
    let program = compile(&schema, "SELECT id FROM t WHERE val IN (5, 20, 999)");
    assert!(uses(&program, Opcode::SeekIndexEq));

    let rows = run_rows(&db, &schema, "SELECT id FROM t WHERE val IN (5, 20, 999)");
    let mut ids: Vec<i64> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Integer(i) => *i,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect();
    ids.sort_unstable();
    // 5 and 20 are present (ids 1 and 4); 999 matches no row.
    assert_eq!(ids, vec![1, 4]);
}

#[test]
fn in_list_with_duplicate_values_does_not_duplicate_the_row() {
    let (db, schema) = int_fixture("in_list_dupes");
    let rows = run_rows(&db, &schema, "SELECT id FROM t WHERE val IN (5, 5, 5)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ---------------------------------------------------------------
// LIKE / GLOB
// ---------------------------------------------------------------

#[test]
fn like_prefix_matches_bare_prefix_and_extended_strings() {
    let (db, schema) = text_fixture("like_prefix_basic");
    let program = compile(&schema, "SELECT id FROM t WHERE name LIKE 'foo%'");
    assert!(uses(&program, Opcode::SeekIndexGE));

    let rows = run_rows(&db, &schema, "SELECT id FROM t WHERE name LIKE 'foo%'");
    let mut ids: Vec<i64> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Integer(i) => *i,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect();
    ids.sort_unstable();
    // 'foo' and 'foobar' match; 'foP'/'far'/'fop'/'zzz' do not.
    assert_eq!(ids, vec![1, 2]);
}

#[test]
fn like_prefix_excludes_the_lexicographic_rollover_row() {
    // 'fop' shares no useful byte-prefix ambiguity with 'foo%' — this
    // guards the upper-bound construction (prefix + char::MAX) against
    // an off-by-one that would incorrectly include the next string in
    // sort order after every 'foo...' string.
    let (db, schema) = text_fixture("like_prefix_rollover");
    let rows = run_rows(&db, &schema, "SELECT id FROM t WHERE name LIKE 'foo%'");
    let ids: Vec<i64> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Integer(i) => *i,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect();
    assert!(!ids.contains(&5), "'fop' must not match 'foo%': {ids:?}");
}

#[test]
fn like_prefix_with_no_matching_rows_is_empty() {
    let (db, schema) = text_fixture("like_prefix_no_match");
    let rows = run_rows(&db, &schema, "SELECT id FROM t WHERE name LIKE 'qux%'");
    assert!(rows.is_empty());
}

#[test]
fn glob_prefix_matches_like_the_asterisk_form() {
    let (db, schema) = text_fixture("glob_prefix_basic");
    let program = compile(&schema, "SELECT id FROM t WHERE name GLOB 'foo*'");
    assert!(uses(&program, Opcode::SeekIndexGE));

    let rows = run_rows(&db, &schema, "SELECT id FROM t WHERE name GLOB 'foo*'");
    let mut ids: Vec<i64> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Integer(i) => *i,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2]);
}

// ---------------------------------------------------------------
// EXPLAIN QUERY PLAN (#606's acceptance criteria: report index usage
// for these query shapes)
// ---------------------------------------------------------------

#[test]
fn explain_query_plan_reports_between_as_a_search_using_index() {
    let (_db, schema) = int_fixture("eqp_between");
    let select = accepted_select("SELECT id FROM t WHERE val BETWEEN 10 AND 20");
    let rows =
        explain_query_plan(&select, &[schema], &std::collections::HashMap::new(), &[]).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].detail.contains("SEARCH") && rows[0].detail.contains("USING INDEX idx_val"),
        "unexpected EQP detail: {}",
        rows[0].detail
    );
}

#[test]
fn explain_query_plan_reports_like_prefix_as_a_search_using_index() {
    let (_db, schema) = text_fixture("eqp_like");
    let select = accepted_select("SELECT id FROM t WHERE name LIKE 'foo%'");
    let rows =
        explain_query_plan(&select, &[schema], &std::collections::HashMap::new(), &[]).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].detail.contains("SEARCH") && rows[0].detail.contains("USING INDEX idx_name"),
        "unexpected EQP detail: {}",
        rows[0].detail
    );
}

#[test]
fn explain_query_plan_reports_in_list_as_a_search_using_index() {
    let (_db, schema) = int_fixture("eqp_in");
    let select = accepted_select("SELECT id FROM t WHERE val IN (5, 20)");
    let rows =
        explain_query_plan(&select, &[schema], &std::collections::HashMap::new(), &[]).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].detail.contains("SEARCH") && rows[0].detail.contains("USING INDEX idx_val"),
        "unexpected EQP detail: {}",
        rows[0].detail
    );
}

fn accepted_select(src: &str) -> sqlite_rs::parser::ast::Select {
    match parse_select(src) {
        ParseOutcome::Accepted(select) => *select,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}
