//! Oracle harness entry point. Run via `make test-corpus`
//! (`cargo test --test corpus`) — kept separate from `make test` so the
//! fast unit-test loop doesn't pay for corpus discovery on every run.
//! See `.openspec/specs/004-corpus/spec.md`.

mod harness;
mod oracle;

mod families_test;
mod harness_test;
mod oracle_test;
mod regen_test;
