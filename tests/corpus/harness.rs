// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Fixture discovery and the real per-fixture reader: opens each
//! committed fixture through the library's `dump` path (issue #37, V1
//! step 9) and reports whether it decoded or failed. See
//! `.openspec/specs/004-corpus/spec.md` Requirement 4.
//!
//! Deliberately does not shell out to `sqlite3` here (per this module's
//! original design: the harness reads committed fixtures only) — actual
//! byte-for-byte oracle diffing against a live `sqlite3` binary lives in
//! `dump_oracle_test.rs`, a separate, explicitly-scoped exception.

use crate::oracle::corpus_dir;
use sqlite_rs::dump::dump_database;
use sqlite_rs::vfs::UnixVfs;
use std::path::PathBuf;

pub const FAMILIES: &[&str] = &[
    "serialtypes",
    "encodings",
    "pagesizes",
    "btrees",
    "features",
    "invalid",
    "journalstates",
];

pub fn discover_fixtures() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for family in FAMILIES {
        let dir = corpus_dir().join(family);
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "db") {
                out.push(path);
            }
        }
    }
    out
}

pub enum FixtureOutcome {
    /// The database opened and every readable table decoded — `tables`
    /// is the count of non-virtual tables read, `warnings` any
    /// gracefully-skipped tables (e.g. virtual tables).
    Dumped {
        tables: usize,
        warnings: Vec<String>,
    },
    /// The database couldn't be opened at all (malformed header, hot
    /// rollback journal, ...) — expected for the `invalid` family and
    /// `journalstates/hot_journal.db`.
    Failed(String),
}

/// Reads `path` end to end through `dump_database`. Never panics: any
/// open/schema/table failure becomes `FixtureOutcome::Failed`.
pub fn read_fixture(path: &std::path::Path) -> FixtureOutcome {
    match dump_database(&UnixVfs, path) {
        Ok(result) => FixtureOutcome::Dumped {
            tables: result.tables.len(),
            warnings: result.warnings,
        },
        Err(e) => FixtureOutcome::Failed(e.to_string()),
    }
}
