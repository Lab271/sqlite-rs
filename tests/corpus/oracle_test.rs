//! Requirement 1 scenarios: the generator script rejects a codec-enabled
//! oracle and a wrong-version oracle. Uses fake `sqlite3` doubles (see
//! `tests/corpus/support/`) rather than real binaries, since the two real
//! binaries this project has access to confound version and codec-ness —
//! isolating each path requires controlling them independently.

use crate::oracle::{gen_fixtures_script, support_dir};
use std::process::Command;

#[test]
fn rejects_codec_oracle() {
    let fake = support_dir().join("fake_sqlite3_codec.sh");
    let output = Command::new(gen_fixtures_script())
        .env("ORACLE_SQLITE3", &fake)
        .output()
        .expect("running gen_fixtures.sh");

    assert!(
        !output.status.success(),
        "expected failure for a codec-enabled oracle"
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        stderr.contains("codec"),
        "stderr should mention codec: {stderr}"
    );
}

#[test]
fn rejects_version_mismatch() {
    let fake = support_dir().join("fake_sqlite3_wrong_version.sh");
    let output = Command::new(gen_fixtures_script())
        .env("ORACLE_SQLITE3", &fake)
        .output()
        .expect("running gen_fixtures.sh");

    assert!(
        !output.status.success(),
        "expected failure for a wrong-version oracle"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("9.99.99"),
        "stderr should name the found version: {stderr}"
    );
}
