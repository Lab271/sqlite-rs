//! Tier 3 — everything else, droppable in the defined order (`plan.md`
//! Core Definition & Drop Order). One ignored stub per drop-order entry,
//! so the drop list itself stays executable: flipping a stub live is the
//! acceptance bar for the ticket that lands that entry.

#[test]
#[ignore = "drop-order 1 (V4) — multi-table read: joins/aggregates"]
fn t3_multi_table_joins_and_aggregates() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 2 (V6) — WAL writing (WAL reading is Tier 0)"]
fn t3_wal_writing_and_live_interop() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 3 (V8) — foreign keys + triggers"]
fn t3_foreign_keys_and_triggers() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 4 (V9) — UPSERT / RETURNING / window functions"]
fn t3_modern_sql_upsert_returning_windows() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 5 — PRAGMAs beyond introspection"]
fn t3_pragmas_beyond_introspection() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 6 — ALTER TABLE, VACUUM"]
fn t3_alter_table_and_vacuum() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 7 (V10) — writing to WITHOUT ROWID / STRICT tables (reading them is Tier 0)"]
fn t3_writes_to_without_rowid_and_strict_tables() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 8 (V11) — vtab/JSON extension semantics: ATTACH, sessions, hooks"]
fn t3_attach_sessions_and_hooks() {
    unimplemented!()
}

#[test]
#[ignore = "drop-order 8 (V12) — FTS5/R-Tree query-level extension semantics"]
fn t3_fts5_and_rtree_query_semantics() {
    unimplemented!()
}
