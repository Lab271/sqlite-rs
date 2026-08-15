//! Tier 2 — WRITE CORE (spec 001-architecture Tier Model, `plan.md` Core
//! Definition): CRUD on rowid tables, basic constraints, rollback-journal
//! transactions, `integrity_check`-clean output. Simplifiable, not
//! droppable — every clause below is a stub today, filling in through
//! V3/V5 per `plan.md`'s Value Blocks table.

#[test]
#[ignore = "V3 — CREATE/INSERT/UPDATE/DELETE round-trip"]
fn t2_crud_round_trips_on_rowid_tables() {
    unimplemented!()
}

#[test]
#[ignore = "V3 — written file passes stock sqlite3 PRAGMA integrity_check"]
fn t2_written_file_passes_integrity_check() {
    unimplemented!()
}

#[test]
#[ignore = "V5 — statement atomicity under failure"]
fn t2_statement_atomicity() {
    unimplemented!()
}

#[test]
#[ignore = "V5 — rollback-journal transactions"]
fn t2_journal_transactions_commit_and_rollback() {
    unimplemented!()
}
