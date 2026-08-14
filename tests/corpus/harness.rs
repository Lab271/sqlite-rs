//! Fixture discovery and the stub reader that V1 steps 1-9 (#5) replace
//! with real decode-and-diff-against-oracle logic. See
//! `.openspec/specs/004-corpus/spec.md` Requirement 4.

use crate::oracle::corpus_dir;
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
    Skipped,
}

/// No reader exists yet — every fixture reports `Skipped`. Real per-fixture
/// decode-and-diff-against-oracle logic lands as V1 steps 1-9 are built.
pub fn read_fixture(_path: &std::path::Path) -> FixtureOutcome {
    FixtureOutcome::Skipped
}
