// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Runs the vendored sqllogictest corpus (#70) — `select1.test` through
//! `select5.test`, and every `evidence/*.test` under
//! `tests/corpus/sql/vendor/sqllogictest/test/` — through
//! [`crate::runner::run_file`], and commits the aggregate pass/skip/
//! fail counts to `tools/sqllogictest-status.json` (the pass-rate
//! number `tools/assurance.py`'s Model section surfaces).
//!
//! Skips green when no pinned oracle is present (spec 004 Requirement
//! 4's policy): this test never fails the suite on a missing oracle,
//! only on an oracle-confirmed divergence in a `query` this engine
//! actually accepted, compiled, and executed.
//!
//! ## Widening past the single-table slice
//!
//! Originally scoped to the 14 files vendored for the V2 single-table
//! slice; widened to 17 by adding `select3-5.test` (V4 joins/subqueries/
//! aggregates — see `tests/corpus/sql/vendor/README.md`) now that V4 has
//! shipped. Still not the full upstream 699-file / ~7.2M-query
//! sqllogictest suite. Widening further needs:
//!
//! - Vendoring (or fetching) the remaining `test/random/**` and
//!   `test/index/**` files `tools/extract_sql_corpus.py` currently
//!   skips as "generated, enormously repetitive" — `make extract-sql-corpus
//!   FETCH=1` already knows how to pull the pinned mirror commit.
//! - This runner's `run_file` already replays arbitrary `statement ok`
//!   setup and skips (not fails) anything outside the engine's
//!   supported grammar/opcodes, so productions from V5+ (transactions,
//!   WAL, CTEs) should just start passing rather than needing runner
//!   changes — *unless* a block adds a genuine write path exercised by
//!   `statement ok` DML, at which point that DML should start running
//!   through this crate's own engine too, not just the oracle, so
//!   writes get scored the same way reads are here.
//! - `tools/sqllogictest-status.json`'s `pass_rate` is computed over
//!   `pass / (pass + fail)` (skips excluded) and `coverage` over
//!   `attempted / queries` — both formulas stay valid at any corpus
//!   size; only the file list and CI runtime budget need revisiting at
//!   699 files. Quote the two together: as coverage climbs toward 1.0
//!   the pass rate is what starts meaning something, and until then a
//!   high pass rate over a small slice of the corpus is not the
//!   headline it looks like.
//! - CI runs this non-gating (`continue-on-error`) while coverage is
//!   low; flipping it to a hard gate is the decision that makes a
//!   divergence block a merge on its own — revisit once the random/
//!   index files are in and coverage is a meaningful fraction of 1.0.

use std::path::PathBuf;

use crate::oracle::{pinned_oracle, skip_no_oracle};
use crate::runner::{run_file, FileTally};

/// The slice vendored by #70 — `select1.test` through `select5.test`, and
/// the `evidence/` files. Pinned so an accidental prune of the vendor
/// directory fails loudly instead of quietly shrinking coverage.
const EXPECTED_FILE_COUNT: usize = 17;

/// Vendored files that legitimately contain no `query` records at all
/// (they exercise DDL this engine has no write path for), and so are
/// exempt from the "every file contributes something" floor below.
const QUERYLESS_FILES: &[&str] = &[
    "slt_lang_createtrigger.test",
    "slt_lang_dropindex.test",
    "slt_lang_droptable.test",
    "slt_lang_droptrigger.test",
    "slt_lang_reindex.test",
];

fn vendor_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/sql/vendor/sqllogictest/test")
}

fn status_json_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/sqllogictest-status.json")
}

/// `select1.test` through `select5.test`, and every `evidence/*.test`, in
/// a fixed (sorted) order so `tools/sqllogictest-status.json` diffs
/// deterministically between runs.
fn discover_files() -> Vec<PathBuf> {
    let dir = vendor_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "test"))
        .collect();

    let evidence_dir = dir.join("evidence");
    files.extend(
        std::fs::read_dir(&evidence_dir)
            .unwrap_or_else(|e| panic!("reading {}: {e}", evidence_dir.display()))
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "test")),
    );

    files.sort();
    files
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn write_status_json(tallies: &[FileTally]) {
    let total_pass: usize = tallies.iter().map(|t| t.pass).sum();
    let total_skip: usize = tallies.iter().map(|t| t.skip).sum();
    let total_suspect: usize = tallies.iter().map(|t| t.suspect).sum();
    let total_fail: usize = tallies.iter().map(|t| t.fail).sum();
    let attempted = total_pass.saturating_add(total_fail);
    let pass_rate = if attempted == 0 {
        0.0
    } else {
        total_pass as f64 / attempted as f64
    };

    let mut json = String::from("{\n  \"files\": [\n");
    for (i, t) in tallies.iter().enumerate() {
        json.push_str(&format!(
            "    {{\"file\": \"{}\", \"pass\": {}, \"skip\": {}, \"suspect\": {}, \"fail\": {}}}",
            json_escape(&t.file),
            t.pass,
            t.skip,
            t.suspect,
            t.fail
        ));
        if i.saturating_add(1) < tallies.len() {
            json.push(',');
        }
        json.push('\n');
    }
    let queries = attempted
        .saturating_add(total_skip)
        .saturating_add(total_suspect);
    let coverage = if queries == 0 {
        0.0
    } else {
        attempted as f64 / queries as f64
    };

    json.push_str("  ],\n");
    // `pass_rate` alone would read as a perfect score while most of the
    // corpus is still skipped as out-of-slice, so `coverage` (the share
    // of queries this engine even attempts) is committed beside it —
    // the honest headline is the pair, not the rate.
    json.push_str(&format!(
        "  \"total\": {{\"pass\": {total_pass}, \"skip\": {total_skip}, \
         \"suspect\": {total_suspect}, \"fail\": {total_fail}, \
         \"queries\": {queries}, \"attempted\": {attempted}, \"pass_rate\": {pass_rate:.4}, \
         \"coverage\": {coverage:.4}}}\n"
    ));
    json.push_str("}\n");

    std::fs::write(status_json_path(), json).unwrap_or_else(|e| {
        panic!("writing {}: {e}", status_json_path().display());
    });
}

/// Green with no fixtures replayed at all if no pinned oracle is
/// present (this crate has no write path, so it cannot build fixture
/// state on its own). A real, unignored `Some(oracle)` run only ever
/// fails on a `query` this engine accepted, compiled, and executed
/// whose output diverges from the file's own expected block — never on
/// a grammar/opcode gap, which is a skip.
#[test]
fn sqllogictest_slice() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("sqllogictest_slice");
        return;
    };

    let files = discover_files();
    assert_eq!(
        files.len(),
        EXPECTED_FILE_COUNT,
        "expected the {EXPECTED_FILE_COUNT} vendored slice files (#70), found {}: \
         re-run `make extract-sql-corpus FETCH=1`",
        files.len()
    );

    let tallies: Vec<FileTally> = files.iter().map(|path| run_file(&oracle, path)).collect();

    write_status_json(&tallies);

    // A truncated or mis-vendored `.test` parses to zero records, which
    // would otherwise report 0/0/0 for that file and still go green —
    // silently dropping corpus coverage with nothing to notice it. Files
    // legitimately contributing no `query` records are named explicitly.
    for tally in &tallies {
        if QUERYLESS_FILES.contains(&tally.file.as_str()) {
            continue;
        }
        assert!(
            tally.pass + tally.skip + tally.suspect + tally.fail > 0,
            "{} yielded no query records at all — truncated or mis-vendored?",
            tally.file
        );
    }

    let suspects: Vec<&str> = tallies
        .iter()
        .flat_map(|t| t.suspects.iter().map(String::as_str))
        .collect();
    assert!(
        suspects.is_empty(),
        "{} quer(ies) this engine declined for a reason that should not \
         happen against oracle-validated input (see Outcome::Suspect):\n{}",
        suspects.len(),
        suspects.join("\n")
    );

    let failures: Vec<&str> = tallies
        .iter()
        .flat_map(|t| t.failures.iter().map(String::as_str))
        .collect();
    assert!(
        failures.is_empty(),
        "{} divergence(s) from the pinned oracle:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
