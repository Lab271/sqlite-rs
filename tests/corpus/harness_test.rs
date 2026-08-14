//! Requirement 4, "Green with stub reader" scenario.

use crate::harness::{discover_fixtures, read_fixture, FixtureOutcome};

#[test]
fn all_fixtures_report_skipped() {
    let fixtures = discover_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no fixtures found — run `make fixtures`"
    );
    for path in &fixtures {
        let FixtureOutcome::Skipped = read_fixture(path);
        println!("SKIPPED: {}", path.display());
    }
}
