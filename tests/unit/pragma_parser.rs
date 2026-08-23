//! Unit tests for the V6 carve-out PRAGMA parser: `PRAGMA journal_mode =
//! WAL|DELETE` only (issue #388, spec 002-parser). Any other pragma name
//! or value is `Unsupported`, not a hard parse error — mirrors `WITH
//! RECURSIVE`'s precedent.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sqlite_rs::parser::ast::*;
use sqlite_rs::parser::{parse_pragma, ParseOutcome};

fn accept(src: &str) -> Pragma {
    match parse_pragma(src) {
        ParseOutcome::Accepted(stmt) => *stmt,
        other => panic!("expected accept for {src:?}, got {other:?}"),
    }
}

fn unsupported(src: &str) {
    match parse_pragma(src) {
        ParseOutcome::Unsupported { .. } => {}
        other => panic!("expected unsupported for {src:?}, got {other:?}"),
    }
}

// ---- accepted -------------------------------------------------------------

#[test]
fn test_accept_journal_mode_wal() {
    let p = accept("PRAGMA journal_mode = WAL");
    assert_eq!(p.journal_mode, PragmaJournalMode::Wal);
}

#[test]
fn test_accept_journal_mode_delete() {
    let p = accept("PRAGMA journal_mode = DELETE");
    assert_eq!(p.journal_mode, PragmaJournalMode::Delete);
}

#[test]
fn test_accept_is_case_insensitive_on_keyword_and_value() {
    let p = accept("pragma JOURNAL_MODE = wal");
    assert_eq!(p.journal_mode, PragmaJournalMode::Wal);

    let p = accept("Pragma Journal_Mode = Delete");
    assert_eq!(p.journal_mode, PragmaJournalMode::Delete);
}

#[test]
fn test_accept_no_surrounding_whitespace_around_eq() {
    let p = accept("PRAGMA journal_mode=WAL");
    assert_eq!(p.journal_mode, PragmaJournalMode::Wal);
}

// ---- unsupported ------------------------------------------------------------

#[test]
fn test_unsupported_pragma_name() {
    unsupported("PRAGMA table_info(t)");
}

#[test]
fn test_unsupported_other_pragma_name() {
    unsupported("PRAGMA cache_size = 10");
}

#[test]
fn test_unsupported_journal_mode_value() {
    unsupported("PRAGMA journal_mode = MEMORY");
    unsupported("PRAGMA journal_mode = OFF");
    unsupported("PRAGMA journal_mode = TRUNCATE");
    unsupported("PRAGMA journal_mode = PERSIST");
}

#[test]
fn test_unsupported_bare_journal_mode_query_form() {
    // `PRAGMA journal_mode;` (no `=`) is real SQLite syntax (queries the
    // current mode) but out of this ticket's narrow carve-out.
    unsupported("PRAGMA journal_mode");
}
