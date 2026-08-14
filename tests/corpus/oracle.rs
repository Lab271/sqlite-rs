//! Pinned-oracle constants shared by the corpus harness and its tests.
//!
//! Enforcement (codec + version checks) happens in `tools/gen_fixtures.sh`
//! at fixture-generation time, not here — the harness reads committed
//! fixtures and never shells out to sqlite3 itself. See
//! `.openspec/specs/004-corpus/spec.md` Requirement 1.

use std::path::{Path, PathBuf};

pub const ORACLE_VERSION: &str = "3.53.3";

pub fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/fixtures")
}

pub fn gen_fixtures_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/gen_fixtures.sh")
}

pub fn support_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/support")
}
