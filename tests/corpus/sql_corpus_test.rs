// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Validates the SQL text corpus's three-way labels
//! (`valid_in_subset` / `valid_out_of_subset` / `invalid`) against a real
//! `sqlite3` — issue #2, scoped per spike #57's V2 grammar-slice findings
//! (`tests/spike/006_grammar_slice/FINDINGS.md`). This does not exercise
//! sqlite-rs's own parser (not implemented yet, see #61); it only proves
//! each corpus statement is labeled the way real SQLite actually treats
//! it, so the corpus is trustworthy once a parser lands.
//!
//! Skips (not fails) when no pinned oracle is available, matching the
//! rest of `tests/corpus`.

use crate::oracle::{pinned_oracle, skip_no_oracle};
use std::path::{Path, PathBuf};
use std::process::Command;

const SCHEMA: &str = "CREATE TABLE t (a INTEGER, b INTEGER, name TEXT); \
CREATE TABLE u (a INTEGER, b INTEGER, name TEXT); \
CREATE INDEX idx_t_b ON t (b);";

fn sql_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/sql")
}

fn statements_in(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("--"))
        .map(str::to_string)
        .collect()
}

fn sql_files_in(subdir: &str) -> Vec<PathBuf> {
    let dir = sql_dir().join(subdir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("sql"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no .sql files found under {}",
        dir.display()
    );
    files
}

fn runs_clean_in_sqlite3(oracle: &Path, statement: &str) -> bool {
    let script = format!("{SCHEMA}\n{statement}\n");
    let output = Command::new(oracle)
        .arg(":memory:")
        .arg(&script)
        .output()
        .unwrap_or_else(|e| panic!("invoking oracle sqlite3: {e}"));
    output.status.success()
}

#[test]
fn valid_in_subset_statements_parse_in_real_sqlite() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("valid_in_subset_statements_parse_in_real_sqlite");
        return;
    };
    for path in sql_files_in("valid_in_subset") {
        for statement in statements_in(&path) {
            assert!(
                runs_clean_in_sqlite3(&oracle, &statement),
                "expected valid_in_subset statement to succeed in real sqlite3 ({}): {statement}",
                path.display()
            );
        }
    }
}

#[test]
fn valid_out_of_subset_statements_parse_in_real_sqlite() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("valid_out_of_subset_statements_parse_in_real_sqlite");
        return;
    };
    for path in sql_files_in("valid_out_of_subset") {
        for statement in statements_in(&path) {
            assert!(
                runs_clean_in_sqlite3(&oracle, &statement),
                "expected valid_out_of_subset statement to succeed in real sqlite3 ({}): {statement}",
                path.display()
            );
        }
    }
}

#[test]
fn invalid_statements_are_rejected_by_real_sqlite() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("invalid_statements_are_rejected_by_real_sqlite");
        return;
    };
    for path in sql_files_in("invalid") {
        for statement in statements_in(&path) {
            assert!(
                !runs_clean_in_sqlite3(&oracle, &statement),
                "expected invalid statement to be rejected by real sqlite3 ({}): {statement}",
                path.display()
            );
        }
    }
}
