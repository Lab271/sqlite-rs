//! Unit tests for the V3 INSERT parser (issue #188, spec 002-parser).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use sqlite_rs::parser::ast::*;
use sqlite_rs::parser::{parse_insert, ParseOutcome};

fn accept(src: &str) -> Insert {
    match parse_insert(src) {
        ParseOutcome::Accepted(insert) => *insert,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn invalid(src: &str) -> String {
    match parse_insert(src) {
        ParseOutcome::Invalid { message, .. } => message,
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

fn unsupported(src: &str) -> String {
    match parse_insert(src) {
        ParseOutcome::Unsupported { message, .. } => message,
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

#[test]
fn test_accept_insert_values() {
    let insert = accept("INSERT INTO t VALUES (1, 2)");
    assert_eq!(insert.table, "t");
    assert_eq!(insert.or_action, None);
    assert_eq!(insert.columns, None);
    let InsertSource::Values(rows) = &insert.source else {
        panic!("expected Values source");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 2);
}

#[test]
fn test_accept_insert_multi_row_values() {
    let insert = accept("INSERT INTO t VALUES (1, 2), (3, 4), (5, 6)");
    let InsertSource::Values(rows) = &insert.source else {
        panic!("expected Values source");
    };
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_accept_insert_with_column_list() {
    let insert = accept("INSERT INTO t (a, b) VALUES (1, 2)");
    assert_eq!(insert.columns, Some(vec!["a".to_string(), "b".to_string()]));
}

#[test]
fn test_accept_insert_default_values() {
    let insert = accept("INSERT INTO t DEFAULT VALUES");
    assert_eq!(insert.source, InsertSource::DefaultValues);
}

#[test]
fn test_accept_insert_select() {
    let insert = accept("INSERT INTO t SELECT * FROM other");
    let InsertSource::Select(select) = &insert.source else {
        panic!("expected Select source");
    };
    assert_eq!(select.from.as_ref().unwrap().first.name(), Some("other"));
}

#[test]
fn test_accept_insert_or_replace() {
    let insert = accept("INSERT OR REPLACE INTO t VALUES (1)");
    assert_eq!(insert.or_action, Some(ConflictAction::Replace));
}

#[test]
fn test_accept_insert_or_ignore() {
    let insert = accept("INSERT OR IGNORE INTO t VALUES (1)");
    assert_eq!(insert.or_action, Some(ConflictAction::Ignore));
}

#[test]
fn test_accept_insert_or_abort() {
    let insert = accept("INSERT OR ABORT INTO t VALUES (1)");
    assert_eq!(insert.or_action, Some(ConflictAction::Abort));
}

#[test]
fn test_accept_insert_or_rollback() {
    let insert = accept("INSERT OR ROLLBACK INTO t VALUES (1)");
    assert_eq!(insert.or_action, Some(ConflictAction::Rollback));
}

#[test]
fn test_accept_insert_or_fail() {
    let insert = accept("INSERT OR FAIL INTO t VALUES (1)");
    assert_eq!(insert.or_action, Some(ConflictAction::Fail));
}

#[test]
fn test_printer_roundtrip_values() {
    let insert = accept("INSERT OR REPLACE INTO t (a, b) VALUES (1, 2), (3, 4)");
    let printed = insert.to_string();
    let reparsed = accept(&printed);
    assert_eq!(insert, reparsed, "printed: {printed}");
}

#[test]
fn test_printer_roundtrip_default_values() {
    let insert = accept("INSERT INTO t DEFAULT VALUES");
    let printed = insert.to_string();
    let reparsed = accept(&printed);
    assert_eq!(insert, reparsed, "printed: {printed}");
}

#[test]
fn test_printer_roundtrip_select() {
    let insert = accept("INSERT INTO t SELECT a, b FROM other WHERE a > 1");
    let printed = insert.to_string();
    let reparsed = accept(&printed);
    assert_eq!(insert, reparsed, "printed: {printed}");
}

#[test]
fn test_invalid_insert_missing_into() {
    invalid("INSERT t VALUES (1)");
}

#[test]
fn test_invalid_insert_missing_source() {
    invalid("INSERT INTO t");
}

#[test]
fn test_invalid_insert_bad_conflict_action() {
    invalid("INSERT OR FOO INTO t VALUES (1)");
}

#[test]
fn test_invalid_insert_unclosed_paren() {
    invalid("INSERT INTO t VALUES (1, 2");
}

#[test]
fn test_invalid_insert_trailing_garbage() {
    invalid("INSERT INTO t VALUES (1) EXTRA");
}

/// #377 made plain `UNION` parse successfully wherever a `select-stmt`
/// grammar production appears — `INSERT ... SELECT`'s source included
/// — the same way `UNION ALL` already did (#240). Parsing accepts it;
/// `compile_insert` rejects the compound source at codegen time
/// instead (`tests/unit/insert_parser.rs` has no codegen access, so
/// see `src/codegen/stmt/insert.rs`'s explicit `select.compound`
/// guard).
#[test]
fn test_insert_select_source_compound_now_parses() {
    let insert = accept("INSERT INTO t SELECT a FROM other UNION SELECT a FROM other");
    let InsertSource::Select(select) = &insert.source else {
        panic!("expected a SELECT source");
    };
    assert_eq!(select.compound.len(), 1);
}

/// `INTERSECT`/`EXCEPT` remain unsupported everywhere, including as an
/// `INSERT ... SELECT` source.
#[test]
fn test_unsupported_insert_select_source_intersect() {
    unsupported("INSERT INTO t SELECT a FROM other INTERSECT SELECT a FROM other");
}
