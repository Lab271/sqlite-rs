//! Unit tests for the V3 DELETE parser (issue #191, spec 002-parser).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use sqlite_rs::parser::ast::*;
use sqlite_rs::parser::{parse_delete, ParseOutcome};

fn accept(src: &str) -> Delete {
    match parse_delete(src) {
        ParseOutcome::Accepted(delete) => *delete,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn invalid(src: &str) -> String {
    match parse_delete(src) {
        ParseOutcome::Invalid { message, .. } => message,
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

fn unsupported(src: &str) -> String {
    match parse_delete(src) {
        ParseOutcome::Unsupported { message, .. } => message,
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

#[test]
fn test_accept_delete_no_where() {
    let delete = accept("DELETE FROM t");
    assert_eq!(delete.table, "t");
    assert_eq!(delete.where_clause, None);
}

#[test]
fn test_accept_delete_with_where() {
    let delete = accept("DELETE FROM t WHERE a > 1");
    assert_eq!(delete.table, "t");
    assert!(delete.where_clause.is_some());
}

#[test]
fn test_printer_roundtrip_no_where() {
    let delete = accept("DELETE FROM t");
    let printed = delete.to_string();
    let reparsed = accept(&printed);
    assert_eq!(delete, reparsed, "printed: {printed}");
}

#[test]
fn test_printer_roundtrip_with_where() {
    let delete = accept("DELETE FROM t WHERE a > 1 AND b = 2");
    let printed = delete.to_string();
    let reparsed = accept(&printed);
    assert_eq!(delete, reparsed, "printed: {printed}");
}

#[test]
fn test_invalid_delete_missing_from() {
    invalid("DELETE t");
}

#[test]
fn test_invalid_delete_missing_table() {
    invalid("DELETE FROM");
}

#[test]
fn test_invalid_delete_trailing_garbage() {
    invalid("DELETE FROM t WHERE a > 1 EXTRA");
}

#[test]
fn test_invalid_delete_limit_not_yet_supported() {
    // LIMIT/ORDER BY on DELETE are out of scope for this ticket (see
    // CHANGELOG) and rejected as trailing garbage, not as `Unsupported` —
    // this locks in that documented scope decision.
    invalid("DELETE FROM t LIMIT 1");
}

#[test]
fn test_unsupported_delete_trailing_compound() {
    // Trailing UNION is only rejected once control returns to the
    // top-level expect_end check (issue #224).
    unsupported("DELETE FROM t UNION SELECT 1");
}

#[test]
fn test_unsupported_delete_where_subquery() {
    // #238 made `IN (SELECT ...)` a generic expression-grammar
    // production shared by every WHERE clause (SELECT, DELETE, UPDATE
    // alike). #251 threaded a table catalog through `compile_delete`,
    // so this now compiles too, not just parses — see
    // `tests/corpus/subquery_test.rs`'s `delete_where_in_subquery_matches_oracle`.
    let delete = accept("DELETE FROM t WHERE a IN (SELECT a FROM t)");
    assert!(matches!(
        delete.where_clause.map(|w| w.kind),
        Some(ExprKind::InSubquery { .. })
    ));
}
