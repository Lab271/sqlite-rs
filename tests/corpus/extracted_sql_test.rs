//! Ratchets sqlite-rs's tokenizer and parser against the extracted external
//! SQL corpus — issue #70, follow-up to #2.
//!
//! `sql_corpus_test.rs` validates the *labels* on #2's hand-curated corpus by
//! asking a real `sqlite3` whether each statement runs. That approach does not
//! transfer here: extracted statements are lifted out of their originating
//! `.test` file and so reference tables, attached databases and triggers that
//! file created (`CREATE TABLE aux.t4(...)`). Replaying them standalone would
//! fail for reasons that say nothing about sqlite-rs. Whole-file replay in
//! original order is #96's job (the sqllogictest slice runner).
//!
//! What *is* checkable statement-by-statement, without a schema and without an
//! oracle, is our own front end. Every statement here was accepted by real
//! SQLite in the suite it came from, which gives two invariants:
//!
//! 1.  **Tokenizer totality** — no statement may produce `TokenKind::Error`.
//!     Real SQLite tokenized all of them; a lexer error is our bug.
//! 2.  **No false syntax errors** — no extracted SELECT may come back
//!     `ParseOutcome::Invalid`. `Accepted` and `Unsupported` are both fine
//!     (the V2 parser is a deliberate grammar slice), but `Invalid` claims
//!     valid SQL is malformed, which is always wrong.
//!
//! Unlike the #2 harness this needs no oracle, so it runs in the default
//! `make test` loop rather than behind `make test-corpus`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sqlite_rs::parser::tokenizer::{TokenKind, Tokenizer};
use sqlite_rs::parser::{parse_select, ParseOutcome};
use std::path::{Path, PathBuf};

/// Categories written by `tools/extract_sql_corpus.py`.
const CATEGORIES: [&str; 5] = ["select", "insert", "update", "delete", "ddl"];

fn sql_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/sql")
}

/// Read one extracted corpus file: one statement per line, `--` comments and
/// blank lines skipped (same convention as `sql_corpus_test.rs`).
fn statements_in(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("--"))
        .map(str::to_string)
        .collect()
}

/// Every extracted `.sql` file for a category, across both source corpora.
fn files_for(category: &str) -> Vec<PathBuf> {
    let dir = sql_dir().join(category);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("sql"))
        .collect();
    files.sort();
    files
}

fn all_statements() -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for category in CATEGORIES {
        for path in files_for(category) {
            for statement in statements_in(&path) {
                out.push((path.clone(), statement));
            }
        }
    }
    out
}

/// The corpus must actually be present and non-trivial — a silently empty
/// extraction would make every other test here vacuously pass.
#[test]
fn extracted_corpus_is_present() {
    let statements = all_statements();
    assert!(
        statements.len() > 3000,
        "expected a substantial extracted corpus, found {} statements — \
         run `make sql-corpus` to regenerate",
        statements.len()
    );
    for category in CATEGORIES {
        assert!(
            !files_for(category).is_empty(),
            "no extracted .sql files for category {category}"
        );
    }
}

/// Invariant 1: the tokenizer is total over real SQLite-accepted SQL.
#[test]
fn every_extracted_statement_tokenizes_without_error() {
    let mut failures = Vec::new();
    for (path, statement) in all_statements() {
        for token in Tokenizer::tokenize(&statement) {
            if let TokenKind::Error(reason) = &token.kind {
                failures.push(format!(
                    "{}: {reason}\n    {statement}",
                    path.file_name().unwrap().to_string_lossy()
                ));
                break;
            }
        }
    }
    assert!(
        failures.is_empty(),
        "tokenizer errored on {} statement(s) real SQLite accepts:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Ceiling for [`no_extracted_select_is_reported_invalid`] — the count of
/// extracted SELECTs the V2 parser currently misreports as `Invalid` when they
/// are valid SQL it merely doesn't implement yet.
///
/// The target is 0. Every one of these is a real diagnostics bug: `Invalid`
/// asserts the SQL is malformed, whereas `Unsupported` is what a
/// recognized-but-unimplemented construct should yield. The known causes,
/// grouped by frequency, are subquery-in-FROM, `IN <table-name>`,
/// schema-qualified names (`aux.t5`, `temp.sqlite_master`), `HAVING` without
/// `GROUP BY`, the `->`/`->>` operators, `NOT INDEXED`, and bare `VALUES`.
///
/// Tracked as a follow-up to #70; lower this number as the parser grows —
/// never raise it. A raise means a regression that reclassified valid SQL as
/// malformed.
const SELECT_INVALID_BASELINE: usize = 131;

/// Invariant 2: the parser must not call real, SQLite-accepted SELECT invalid.
/// `Unsupported` is expected and fine — the V2 grammar is a deliberate slice.
///
/// Enforced as a downward ratchet against [`SELECT_INVALID_BASELINE`] rather
/// than at zero, so the existing misclassifications stay visible and counted
/// instead of being silently tolerated.
#[test]
fn no_extracted_select_is_reported_invalid() {
    let mut failures = Vec::new();
    let mut accepted = 0usize;
    let mut unsupported = 0usize;

    for path in files_for("select") {
        for statement in statements_in(&path) {
            match parse_select(&statement) {
                ParseOutcome::Accepted(_) => accepted += 1,
                ParseOutcome::Unsupported { .. } => unsupported += 1,
                ParseOutcome::Invalid { message, span } => failures.push(format!(
                    "{} [line {} col {}]: {message}\n    {statement}",
                    path.file_name().unwrap().to_string_lossy(),
                    span.line,
                    span.column
                )),
            }
        }
    }

    println!(
        "extracted SELECT: {accepted} accepted, {unsupported} unsupported, \
         {} invalid (baseline {SELECT_INVALID_BASELINE})",
        failures.len()
    );

    assert!(
        failures.len() <= SELECT_INVALID_BASELINE,
        "parser reported {} valid SELECT statement(s) as Invalid, above the \
         baseline of {SELECT_INVALID_BASELINE} — this is a regression that \
         reclassified valid SQL as malformed:\n{}",
        failures.len(),
        failures.join("\n")
    );

    assert!(
        failures.len() >= SELECT_INVALID_BASELINE,
        "parser now misreports only {} SELECT(s) as Invalid, below the \
         baseline of {SELECT_INVALID_BASELINE} — good news: lower \
         SELECT_INVALID_BASELINE to {} to lock the improvement in.",
        failures.len(),
        failures.len()
    );
}
