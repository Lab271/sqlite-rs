//! sqllogictest slice runner entry point. Run via `make sqllogictest`
//! (`cargo test --test sqllogictest`) — kept separate from `make test`
//! (same rationale as `corpus`/`parity`, see Cargo.toml): it shells out
//! to the pinned oracle to build each vendored `.test` file's fixture
//! state. See `.openspec/specs/004-corpus/spec.md` Requirement 4 and
//! `runner_test.rs`'s doc comment for the V4 (full-suite) handover.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

// Reused as-is from `tests/corpus/` for `pinned_oracle`/`skip_no_oracle`
// only — the CSV/list rendering helpers alongside them are unused here.
#[path = "../corpus/oracle.rs"]
#[allow(dead_code)]
mod oracle;

mod format;
mod runner;
mod runner_test;
