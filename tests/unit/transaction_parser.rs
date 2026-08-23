//! Unit tests for the V5 transaction-control parser: BEGIN/COMMIT/ROLLBACK
//! (issue #356, spec 002-parser). Parsing only — codegen/execution of
//! transactions is later V5 scope.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sqlite_rs::parser::ast::*;
use sqlite_rs::parser::{parse_begin, parse_commit, parse_rollback, ParseOutcome};

fn accept_begin(src: &str) -> Begin {
    match parse_begin(src) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn accept_commit(src: &str) -> Commit {
    match parse_commit(src) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn accept_rollback(src: &str) -> Rollback {
    match parse_rollback(src) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn invalid_begin(src: &str) {
    match parse_begin(src) {
        ParseOutcome::Invalid { .. } => {}
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

fn invalid_commit(src: &str) {
    match parse_commit(src) {
        ParseOutcome::Invalid { .. } => {}
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

fn invalid_rollback(src: &str) {
    match parse_rollback(src) {
        ParseOutcome::Invalid { .. } => {}
        other => panic!("expected invalid for {src:?}, got {other:?}"),
    }
}

fn unsupported_begin(src: &str) {
    match parse_begin(src) {
        ParseOutcome::Unsupported { .. } => {}
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

fn unsupported_commit(src: &str) {
    match parse_commit(src) {
        ParseOutcome::Unsupported { .. } => {}
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

fn unsupported_rollback(src: &str) {
    match parse_rollback(src) {
        ParseOutcome::Unsupported { .. } => {}
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

// ---- BEGIN ------------------------------------------------------------

#[test]
fn test_accept_begin_bare() {
    let b = accept_begin("BEGIN");
    assert_eq!(b.mode, None);
}

#[test]
fn test_accept_begin_transaction() {
    let b = accept_begin("BEGIN TRANSACTION");
    assert_eq!(b.mode, None);
}

#[test]
fn test_accept_begin_deferred() {
    let b = accept_begin("BEGIN DEFERRED");
    assert_eq!(b.mode, Some(TransactionMode::Deferred));
}

#[test]
fn test_accept_begin_deferred_transaction() {
    let b = accept_begin("BEGIN DEFERRED TRANSACTION");
    assert_eq!(b.mode, Some(TransactionMode::Deferred));
}

#[test]
fn test_accept_begin_immediate() {
    let b = accept_begin("BEGIN IMMEDIATE");
    assert_eq!(b.mode, Some(TransactionMode::Immediate));
}

#[test]
fn test_accept_begin_exclusive() {
    let b = accept_begin("BEGIN EXCLUSIVE");
    assert_eq!(b.mode, Some(TransactionMode::Exclusive));
}

#[test]
fn test_invalid_begin_trailing_garbage() {
    invalid_begin("BEGIN EXTRA");
}

#[test]
fn test_invalid_begin_bad_mode() {
    invalid_begin("BEGIN FOO TRANSACTION");
}

/// `expect_end`'s trailing-token check treats a leftover `UNION` (etc.)
/// keyword as an unimplemented-but-recognized construct rather than a
/// plain syntax error, regardless of which statement precedes it — the
/// same shared classification `ddl_parser.rs`'s `DROP TABLE ... UNION
/// SELECT 1` test relies on for `parse_drop_table`.
#[test]
fn test_unsupported_begin_trailing_union() {
    unsupported_begin("BEGIN UNION SELECT 1");
}

// ---- COMMIT / END -------------------------------------------------------

#[test]
fn test_accept_commit_bare() {
    accept_commit("COMMIT");
}

#[test]
fn test_accept_commit_transaction() {
    accept_commit("COMMIT TRANSACTION");
}

#[test]
fn test_accept_end() {
    accept_commit("END");
}

#[test]
fn test_accept_end_transaction() {
    accept_commit("END TRANSACTION");
}

#[test]
fn test_invalid_commit_trailing_garbage() {
    invalid_commit("COMMIT EXTRA");
}

#[test]
fn test_unsupported_commit_trailing_union() {
    unsupported_commit("COMMIT UNION SELECT 1");
}

// ---- ROLLBACK -----------------------------------------------------------

#[test]
fn test_accept_rollback_bare() {
    accept_rollback("ROLLBACK");
}

#[test]
fn test_accept_rollback_transaction() {
    accept_rollback("ROLLBACK TRANSACTION");
}

#[test]
fn test_invalid_rollback_trailing_garbage() {
    invalid_rollback("ROLLBACK EXTRA");
}

#[test]
fn test_unsupported_rollback_trailing_union() {
    unsupported_rollback("ROLLBACK UNION SELECT 1");
}

// ---- printer round-trip ---------------------------------------------------

#[test]
fn test_printer_roundtrip_begin_bare() {
    let b = accept_begin("BEGIN");
    let printed = b.to_string();
    let reparsed = accept_begin(&printed);
    assert_eq!(b, reparsed, "printed: {printed}");
}

#[test]
fn test_printer_roundtrip_begin_deferred() {
    let b = accept_begin("BEGIN DEFERRED");
    let printed = b.to_string();
    let reparsed = accept_begin(&printed);
    assert_eq!(b, reparsed, "printed: {printed}");
}

#[test]
fn test_printer_roundtrip_begin_immediate() {
    let b = accept_begin("BEGIN IMMEDIATE");
    let printed = b.to_string();
    let reparsed = accept_begin(&printed);
    assert_eq!(b, reparsed, "printed: {printed}");
}

#[test]
fn test_printer_roundtrip_begin_exclusive() {
    let b = accept_begin("BEGIN EXCLUSIVE");
    let printed = b.to_string();
    let reparsed = accept_begin(&printed);
    assert_eq!(b, reparsed, "printed: {printed}");
}

#[test]
fn test_printer_roundtrip_commit() {
    let c = accept_commit("COMMIT");
    let printed = c.to_string();
    let reparsed = accept_commit(&printed);
    assert_eq!(c, reparsed, "printed: {printed}");
}

#[test]
fn test_printer_roundtrip_rollback() {
    let r = accept_rollback("ROLLBACK");
    let printed = r.to_string();
    let reparsed = accept_rollback(&printed);
    assert_eq!(r, reparsed, "printed: {printed}");
}
