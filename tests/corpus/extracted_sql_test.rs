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
//!
//! The corpus-presence and tokenizer-totality invariants are checked once
//! per source corpus (`tcl`/`sqllogictest` — the two subdirectory-less `.sql`
//! filenames `tools/extract_sql_corpus.py` writes per category) rather than
//! once over both combined, so `make test-tcl`/`make test-sqllogictest` can
//! select just one via cargo's test-name substring filter (`_tcl`/
//! `_sqllogictest`) without a second test binary. The SELECT-invalid ratchet
//! ([`SELECT_INVALID_BASELINE`]) stays combined — its baseline count was
//! derived and is documented against the combined corpus, and splitting it
//! would require re-deriving two baselines for no invariant-strength gain.

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
    files_for_source(category, None)
}

/// Extracted `.sql` file(s) for a category, optionally restricted to one
/// source corpus (`"tcl"` or `"sqllogictest"` — matches the filename stem).
fn files_for_source(category: &str, source: Option<&str>) -> Vec<PathBuf> {
    let dir = sql_dir().join(category);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("sql"))
        .filter(|p| match source {
            None => true,
            Some(s) => p.file_stem().and_then(|s| s.to_str()) == Some(s),
        })
        .collect();
    files.sort();
    files
}

fn all_statements_for(source: Option<&str>) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for category in CATEGORIES {
        for path in files_for_source(category, source) {
            for statement in statements_in(&path) {
                out.push((path.clone(), statement));
            }
        }
    }
    out
}

/// The corpus must actually be present and non-trivial for each source — a
/// silently empty extraction would make every other test here vacuously
/// pass. Named per-source (`_tcl`/`_sqllogictest`) so `make test-tcl`/
/// `make test-sqllogictest` (cargo's test-name substring filter) can select
/// just one.
fn assert_corpus_present(source: &str, min_statements: usize) {
    let statements = all_statements_for(Some(source));
    assert!(
        statements.len() > min_statements,
        "expected a substantial extracted {source} corpus, found {} statements — \
         run `make extract-sql-corpus` to regenerate",
        statements.len()
    );
    for category in CATEGORIES {
        assert!(
            !files_for_source(category, Some(source)).is_empty(),
            "no extracted {source} .sql file for category {category}"
        );
    }
}

#[test]
fn extracted_corpus_is_present_tcl() {
    assert_corpus_present("tcl", 2000);
}

#[test]
fn extracted_corpus_is_present_sqllogictest() {
    assert_corpus_present("sqllogictest", 1000);
}

/// Invariant 1: the tokenizer is total over real SQLite-accepted SQL.
fn assert_tokenizes_without_error(source: &str) {
    let mut failures = Vec::new();
    for (path, statement) in all_statements_for(Some(source)) {
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
        "tokenizer errored on {} {source} statement(s) real SQLite accepts:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_extracted_tcl_statement_tokenizes_without_error() {
    assert_tokenizes_without_error("tcl");
}

#[test]
fn every_extracted_sqllogictest_statement_tokenizes_without_error() {
    assert_tokenizes_without_error("sqllogictest");
}

/// The parser must bound its own recursion rather than exhaust the stack: a
/// stack overflow aborts the process (uncatchable, `SIGABRT`), which would
/// contradict spec 005 Requirement 2's no-panic totality claim and is
/// reachable from arbitrary SQL input.
///
/// Real sqlite3 accepts 61 levels and rejects far deeper input with
/// `Parse error: Recursion limit` (its `SQLITE_MAX_EXPR_DEPTH` is 1000); we
/// accept 61 and reject past ~67 (`MAX_EXPR_DEPTH` divided across the three
/// depth-guarded recursion points). The bound differing from SQLite's is a
/// deliberate, documented divergence (ADR 0013, #118) rather than an
/// accident — that the guard fires *at all*, on a default-size thread
/// stack, without aborting, is what this test pins. Runs directly on the
/// default test thread (no oversized stack needed): #118 cut the parser's
/// own per-level stack cost by collapsing the OR/AND and relational-
/// through-concat precedence levels into two precedence-climbing loops
/// (`bool_expr`, `binary_expr`), rather than papering over the cost with a
/// bigger thread.
#[test]
fn deeply_nested_expressions_hit_the_depth_guard_instead_of_the_stack() {
    // 61 levels: accepted by real sqlite3, and present in the corpus.
    let ok = format!("SELECT {}1{}", "abs(".repeat(61), ")".repeat(61));
    assert!(
        !matches!(parse_select(&ok), ParseOutcome::Invalid { .. }),
        "61 levels of nesting is valid SQL that real sqlite3 accepts"
    );

    // Pathological input must come back as a diagnostic, not an abort.
    for depth in [200usize, 5_000] {
        let deep = format!("SELECT {}1{}", "abs(".repeat(depth), ")".repeat(depth));
        assert!(
            matches!(parse_select(&deep), ParseOutcome::Invalid { .. }),
            "{depth} levels of nesting must be rejected by the depth guard"
        );
    }
}

/// Ceiling for [`no_extracted_select_is_reported_invalid`] — the count of
/// extracted SELECTs the V2 parser currently misreports as `Invalid` when they
/// are valid SQL it merely doesn't implement yet.
///
/// The target is 0. Every one of these is a real diagnostics bug: `Invalid`
/// asserts the SQL is malformed, whereas `Unsupported` is what a
/// recognized-but-unimplemented construct should yield. The known causes,
/// #113 fixed the bulk of these (#110), taking the count from 131 to 7;
/// #239 fixed the two `GROUP BY` cases, taking it to 5. #375/#377 (V6.1's
/// `WITH`/`UNION` parsing) made three more pre-existing misclassifications
/// reachable — a quoted alias on a plain-`UNION` arm, `[NOT] MATERIALIZED`
/// after `WITH cte AS`, and a `WITH`-clause feeding `INSERT` instead of
/// `SELECT` — all fixed in the same PR (#403) that made them reachable,
/// which also fixed the single-quoted-alias bug at its root (`opt_alias`)
/// instead of leaving it to keep resurfacing every time a future ticket
/// parses one arm/branch deeper, taking the count from 8 to 3. The
/// remainder, all valid SQL real sqlite3 accepts:
///
/// - `temp.sqlite_master` — schema-qualified name with a keyword schema
/// - `SELECT (VALUES(1),(2))` — VALUES in expression position
/// - `SELECT release FROM savepoint` — non-reserved keywords used as
///   identifiers (SQLite's `%fallback ID`)
///
/// Tracked by #110 (follow-up to #70); lower this number as the parser grows —
/// never raise it without a documented cause like the #240/#257/#403 bumps
/// above. A raise means a regression that reclassified valid SQL as
/// malformed.
const SELECT_INVALID_BASELINE: usize = 3;

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
