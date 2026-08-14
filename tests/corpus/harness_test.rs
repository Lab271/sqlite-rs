//! Requirement 4 scenarios: every fixture reports a real outcome (not
//! the old "always Skipped" stub) — the `invalid` family and the
//! hot-journal fixture are expected failures, everything else must
//! decode cleanly.

use crate::harness::{discover_fixtures, read_fixture, FixtureOutcome};

/// Fixtures outside the `invalid` family that are still expected to
/// fail to open — by design, not by gap. Currently just the hot rollback
/// journal fixture (opening it at all would risk serving pre-rollback
/// pages as committed data).
fn expected_to_fail(path: &std::path::Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some("hot_journal.db")
}

#[test]
fn every_fixture_reports_a_real_outcome() {
    let fixtures = discover_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no fixtures found — run `make fixtures`"
    );

    for path in &fixtures {
        let in_invalid_family = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("invalid");
        let should_fail = in_invalid_family || expected_to_fail(path);

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
