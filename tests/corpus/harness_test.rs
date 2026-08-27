// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Requirement 4 scenarios: every fixture reports a real outcome (not
//! the old "always Skipped" stub) — the `invalid` family is expected
//! failures, everything else must decode cleanly.

use crate::harness::{discover_fixtures, read_fixture, FixtureOutcome};

/// `journalstates/hot_journal.db` is deliberately excluded from this
/// generic sweep, not just given a different expected outcome: opening
/// it now recovers a hot journal in place (#172) — replaying its pages
/// into the main file and deleting the journal — which would mutate the
/// checked-in fixture on every test run. Its own dedicated coverage
/// (`tests/tiers/tier0.rs::t0_hot_journal_recovers_committed_state`,
/// `src/pager.rs`'s `hot_journal_fixture_recovers_committed_state`) works
/// against a scratch-temp-dir copy instead.
fn is_hot_journal_fixture(path: &std::path::Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some("hot_journal.db")
}

#[test]
fn every_fixture_reports_a_real_outcome() {
    let fixtures = discover_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no fixtures found — run `make fixtures`"
    );

    for path in fixtures.iter().filter(|p| !is_hot_journal_fixture(p)) {
        let should_fail = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("invalid");

        match read_fixture(path) {
            FixtureOutcome::Dumped { tables, warnings } => {
                assert!(
                    !should_fail,
                    "{}: expected to fail but dumped {tables} tables (warnings: {warnings:?})",
                    path.display()
                );
                println!(
                    "DUMPED: {} ({tables} tables, {} warnings)",
                    path.display(),
                    warnings.len()
                );
            }
            FixtureOutcome::Failed(e) => {
                assert!(
                    should_fail,
                    "{}: expected to dump cleanly but failed: {e}",
                    path.display()
                );
                println!("FAILED (expected): {} — {e}", path.display());
            }
        }
    }
}
