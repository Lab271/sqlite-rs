//! Oracle harness entry point. Run via `make test-corpus`
//! (`cargo test --test corpus`) — kept separate from `make test` so the
//! fast unit-test loop doesn't pay for corpus discovery on every run.
//! See `.openspec/specs/004-corpus/spec.md`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

mod harness;
mod oracle;

mod begin_immediate_lock_interop_test;
mod btree_delete_test;
mod btree_index_insert_delete_test;
mod btree_insert_test;
mod btree_test;
mod cli_e2e_test;
mod cli_write_test;
mod crash_torture_test;
mod dump_oracle_test;
mod expr_vectors_test;
mod families_test;
mod harness_test;
mod index_maintenance_test;
mod index_ordered_group_by_test;
mod index_ordered_scan_test;
mod join_test;
mod journal_interop_test;
mod lock_state_interop_test;
mod oracle_test;
mod pager_write_test;
mod parser_oracle_test;
mod regen_test;
mod repl_test;
mod schema_test;
mod sql_corpus_test;
mod subquery_test;
mod transaction_oracle_test;
mod union_test;
mod unique_constraint_test;
