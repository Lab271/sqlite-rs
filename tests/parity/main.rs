//! Parity harness entry point. Run via `make test-parity`
//! (`cargo test --test parity`) — the third leg of the testing triad
//! alongside tier contracts (#69) and the fixture corpus (#72's Refs:
//! `.openspec/specs/001-architecture/spec.md` Req 3,
//! `.openspec/specs/004-corpus/spec.md` Req 4).
//!
//! Where `tests/corpus` diffs against committed fixtures and a pinned
//! oracle at the library level, this suite runs the SAME statement
//! against both engines side by side, one integration target per value
//! block (`v01.rs` … `v12.rs`), comparing five dimensions per case (see
//! `driver.rs`). See issue #72.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

// Reuses tests/corpus's oracle helpers verbatim rather than duplicating
// them: same pinned sqlite3, same skip-not-fail discipline, same
// `ORACLE_SQLITE3` override.
// Shared verbatim with tests/corpus/main.rs; #[allow(dead_code)] because
// this binary only exercises a subset of each module's helpers.
#[allow(dead_code)]
#[path = "../corpus/harness.rs"]
mod harness;
#[allow(dead_code)]
#[path = "../corpus/oracle.rs"]
mod oracle;

// Unused until V2 activates a query engine to run ParityCase.sql through
// (v01 mirrors dump/schema paths directly, same as tests/corpus does,
// since there's no SQL execution to drive at V1).
#[allow(dead_code)]
mod driver;

mod v01;
mod v02;
mod v03;
mod v04;
mod v05;
mod v06;
mod v07;
mod v08;
mod v09;
mod v10;
mod v11;
mod v12;
