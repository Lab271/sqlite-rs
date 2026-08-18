//! Unit tests for the V3 DELETE parser (issue #191, spec 002-parser).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use sqlite_rs::parser::ast::*;
use sqlite_rs::parser::{parse_delete, DeleteOutcome};

fn accept(src: &str) -> Delete {
    match parse_delete(src) {
        DeleteOutcome::Accepted(delete) => *delete,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn invalid(src: &str) -> String {
    match parse_delete(src) {
        DeleteOutcome::Invalid { message, .. } => message,
        other => panic!("expected invalid for {src:?}, got {other:?}"),
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
