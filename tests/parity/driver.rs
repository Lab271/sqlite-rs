//! Runs one SQL case against both engines and compares it across five
//! dimensions. See issue #72 for the rationale behind each dimension's
//! gating.
//!
//! Every dimension is `Skipped` rather than `Fail`ed when the sqlite-rs
//! side doesn't implement the needed capability yet — same discipline as
//! `tests/corpus/harness.rs` (`.openspec/specs/004-corpus/spec.md` Req 4:
//! "runs green from day one").

use crate::oracle::{pinned_oracle, run_oracle, skip_no_oracle};
use sqlite_rs::vfs::UnixVfs;
use std::path::Path;

/// A `run_case`-shaped query runner: given a fixture path and SQL,
/// returns rendered output lines or an error message.
pub type QueryRunner<'a> = &'a dyn Fn(&Path, &str) -> Result<Vec<String>, String>;

/// One SQL statement to run against both engines on the same fixture DB.
pub struct ParityCase {
    pub name: &'static str,
    pub sql: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DimResult {
    Match,
    Mismatch { ours: String, theirs: String },
    Skipped(&'static str),
}

/// The five comparison dimensions from #72. VM instructions is
/// informational-only (never gates a test) — it is reported separately
/// via [`vm_diff_artifact`], not through [`DimResult`].
pub struct ParityReport {
    pub acceptance: DimResult,
    pub output: DimResult,
    pub schema: DimResult,
}

/// Runs `case` against the pinned oracle and, when a `mine` closure is
/// supplied, against sqlite-rs — otherwise every gated dimension reports
/// [`DimResult::Skipped`] with the given reason (the v02–v12 stub path).
pub fn run_case(db: &Path, case: &ParityCase, mine: Option<QueryRunner>) -> Option<ParityReport> {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle(case.name);
        return None;
    };

    let Some(mine) = mine else {
        return Some(ParityReport {
            acceptance: DimResult::Skipped("not yet implemented"),
            output: DimResult::Skipped("not yet implemented"),
            schema: DimResult::Skipped("not yet implemented"),
        });
    };

    let theirs_raw = run_oracle(&oracle, db, &["-list", "-separator", "|"], case.sql);
    let theirs: Vec<String> = theirs_raw.lines().map(str::to_owned).collect();

    let acceptance_and_output = match mine(db, case.sql) {
        Ok(ours) if ours == theirs => (DimResult::Match, DimResult::Match),
        Ok(ours) => (
            DimResult::Match,
            DimResult::Mismatch {
                ours: ours.join("\n"),
                theirs: theirs.join("\n"),
            },
        ),
        Err(e) => (
            DimResult::Mismatch {
                ours: e,
                theirs: "accepted".to_owned(),
            },
            DimResult::Skipped("acceptance mismatch"),
        ),
    };

    Some(ParityReport {
        acceptance: acceptance_and_output.0,
        output: acceptance_and_output.1,
        schema: schema_dimension(&oracle, db),
    })
}

/// `.schema` text comparison — deferred to whichever v-block turns it on;
/// no sqlite-rs `.schema`-equivalent is wired into this driver yet, so it
/// always skips for now. Kept as a distinct function so a later change
/// only touches this one spot.
fn schema_dimension(_oracle: &Path, _db: &Path) -> DimResult {
    DimResult::Skipped("schema dimension not wired up yet")
}

/// Opens `db` read-only through sqlite-rs's own VFS — the counterpart to
/// `run_oracle`'s `-readonly` sqlite3 invocation. v-block test files pass
/// this (or their own capability-specific closure) as `run_case`'s `mine`
/// argument once the relevant execution path exists.
pub fn unix_vfs_open_ok(db: &Path) -> bool {
    sqlite_rs::dump::dump_database(&UnixVfs, db).is_ok()
}
