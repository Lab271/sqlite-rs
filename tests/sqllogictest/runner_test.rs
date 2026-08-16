//! Runs the vendored sqllogictest corpus (#70) — `select1.test`,
//! `select2.test`, and every `evidence/*.test` under
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
//! ## V4 handover
//!
//! This runner is deliberately scoped to the 14 files vendored for the
//! V2 single-table slice (see `tests/corpus/sql/vendor/README.md`) —
//! not the full upstream 699-file / ~7.2M-query sqllogictest suite.
//! When V4 lifts the single-table restriction, widening this to the
//! full suite needs:
//!
//! - Vendoring (or fetching) the remaining `test/random/**` and
//!   `test/index/**` files `tools/extract_sql_corpus.py` currently
//!   skips as "generated, enormously repetitive" — `make sql-corpus
//!   FETCH=1` already knows how to pull the pinned mirror commit.
//! - This runner's `run_file` already replays arbitrary `statement ok`
//!   setup and skips (not fails) anything outside the engine's
//!   supported grammar/opcodes, so the multi-table/join/subquery
//!   productions V3/V4 add should just start passing rather than
//!   needing runner changes — *unless* V3/V4 adds a genuine write path,
//!   at which point `statement ok` DML should start running through
//!   this crate's own engine too, not just the oracle, so writes get
//!   scored the same way reads are here.
//! - `tools/sqllogictest-status.json`'s `pass_rate` is computed over
//!   `pass / (pass + fail)` (skips excluded) and `coverage` over
//!   `attempted / queries` — both formulas stay valid at any corpus
//!   size; only the file list and CI runtime budget need revisiting at
//!   699 files. Quote the two together: as coverage climbs toward 1.0
//!   the pass rate is what starts meaning something, and until then a
//!   100% pass rate over 7% of the corpus is not the headline it looks
//!   like.
//! - CI runs this non-gating (`continue-on-error`) while coverage is
//!   low; flipping it to a hard gate is the V4-era decision that makes
//!   a divergence block a merge on its own.

use std::path::PathBuf;

use crate::oracle::{pinned_oracle, skip_no_oracle};
use crate::runner::{run_file, FileTally};

fn vendor_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/sql/vendor/sqllogictest/test")
}

fn status_json_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/sqllogictest-status.json")
}

/// `select1.test`, `select2.test`, and every `evidence/*.test`, in a
/// fixed (sorted) order so `tools/sqllogictest-status.json` diffs
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
            "    {{\"file\": \"{}\", \"pass\": {}, \"skip\": {}, \"fail\": {}}}",
            json_escape(&t.file),
            t.pass,
            t.skip,
            t.fail
        ));
        if i.saturating_add(1) < tallies.len() {
            json.push(',');
        }
        json.push('\n');
    }
    let queries = attempted.saturating_add(total_skip);
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
        "  \"total\": {{\"pass\": {total_pass}, \"skip\": {total_skip}, \"fail\": {total_fail}, \
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

    let tallies: Vec<FileTally> = discover_files()
        .iter()
        .map(|path| run_file(&oracle, path))
        .collect();

    write_status_json(&tallies);

    let mut failures: Vec<&str> = Vec::new();
    for tally in &tallies {
        for failure in &tally.failures {
            failures.push(failure);
        }
    }
    assert!(
        failures.is_empty(),
        "{} divergence(s) from the pinned oracle:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
