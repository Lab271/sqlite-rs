// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the PRAGMA parser carve-outs: `journal_mode` (#388),
//! `integrity_check`/`quick_check` (#540, #541), `synchronous` (#645).
//! Any other pragma name or value is `Unsupported`, not a hard parse
//! error -- mirrors `WITH RECURSIVE`'s precedent.

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

fn journal_mode(p: &Pragma) -> PragmaJournalMode {
    match p {
        Pragma::JournalMode { journal_mode, .. } => *journal_mode,
        other => panic!("expected JournalMode, got {other:?}"),
    }
}

fn synchronous_level(p: &Pragma) -> Option<PragmaSynchronous> {
    match p {
        Pragma::Synchronous { level, .. } => *level,
        other => panic!("expected Synchronous, got {other:?}"),
    }
}

// ---- accepted: journal_mode ------------------------------------------------

#[test]
fn test_accept_journal_mode_wal() {
    let p = accept("PRAGMA journal_mode = WAL");
    assert_eq!(journal_mode(&p), PragmaJournalMode::Wal);
}

#[test]
fn test_accept_journal_mode_delete() {
    let p = accept("PRAGMA journal_mode = DELETE");
    assert_eq!(journal_mode(&p), PragmaJournalMode::Delete);
}

#[test]
fn test_accept_is_case_insensitive_on_keyword_and_value() {
    let p = accept("pragma JOURNAL_MODE = wal");
    assert_eq!(journal_mode(&p), PragmaJournalMode::Wal);

    let p = accept("Pragma Journal_Mode = Delete");
    assert_eq!(journal_mode(&p), PragmaJournalMode::Delete);
}

#[test]
fn test_accept_no_surrounding_whitespace_around_eq() {
    let p = accept("PRAGMA journal_mode=WAL");
    assert_eq!(journal_mode(&p), PragmaJournalMode::Wal);
}

// ---- accepted: integrity_check / quick_check -------------------------------

#[test]
fn test_accept_integrity_check() {
    let p = accept("PRAGMA integrity_check");
    assert_eq!(
        p,
        Pragma::IntegrityCheck {
            quick: false,
            span: p.span()
        }
    );
}

#[test]
fn test_accept_quick_check() {
    let p = accept("PRAGMA quick_check");
    assert_eq!(
        p,
        Pragma::IntegrityCheck {
            quick: true,
            span: p.span()
        }
    );
}

#[test]
fn test_accept_integrity_check_is_case_insensitive() {
    let p = accept("pragma Integrity_Check");
    assert_eq!(
        p,
        Pragma::IntegrityCheck {
            quick: false,
            span: p.span()
        }
    );
}

// ---- accepted: synchronous --------------------------------------------------

#[test]
fn test_accept_synchronous_query_form() {
    let p = accept("PRAGMA synchronous");
    assert_eq!(synchronous_level(&p), None);
}

#[test]
fn test_accept_synchronous_off() {
    let p = accept("PRAGMA synchronous = OFF");
    assert_eq!(synchronous_level(&p), Some(PragmaSynchronous::Off));
}

#[test]
fn test_accept_synchronous_normal() {
    let p = accept("PRAGMA synchronous = NORMAL");
    assert_eq!(synchronous_level(&p), Some(PragmaSynchronous::Normal));
}

#[test]
fn test_accept_synchronous_full() {
    let p = accept("PRAGMA synchronous = FULL");
    assert_eq!(synchronous_level(&p), Some(PragmaSynchronous::Full));
}

#[test]
fn test_accept_synchronous_integer_values() {
    let p = accept("PRAGMA synchronous = 0");
    assert_eq!(synchronous_level(&p), Some(PragmaSynchronous::Off));
    let p = accept("PRAGMA synchronous = 1");
    assert_eq!(synchronous_level(&p), Some(PragmaSynchronous::Normal));
    let p = accept("PRAGMA synchronous = 2");
    assert_eq!(synchronous_level(&p), Some(PragmaSynchronous::Full));
}

#[test]
fn test_accept_synchronous_is_case_insensitive() {
    let p = accept("pragma SYNCHRONOUS = full");
    assert_eq!(synchronous_level(&p), Some(PragmaSynchronous::Full));
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

#[test]
fn test_unsupported_integrity_check_with_arg() {
    unsupported("PRAGMA integrity_check(10)");
}

#[test]
fn test_unsupported_synchronous_value() {
    unsupported("PRAGMA synchronous = ON");
    unsupported("PRAGMA synchronous = EXTRA");
    unsupported("PRAGMA synchronous = 3");
    unsupported("PRAGMA synchronous = 4");
}
