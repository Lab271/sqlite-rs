//! #196 acceptance: `compile_insert`/`compile_delete`/`compile_update`
//! keep secondary indexes in sync with table data. Seeds a table +
//! index via the oracle (so index root pages exist), reads its
//! `TableSchema` (#211's `indexes` catalog) via `read_schema`, then runs
//! our own codegen through `execute_with_writable_db` and checks the
//! oracle's own `PRAGMA integrity_check` — which specifically validates
//! that every index has exactly the row set its table does — plus an
//! indexed `SELECT` to confirm the index is actually usable for lookups
//! (a corrupt/stale index that integrity_check somehow missed would
//! still answer these wrong).

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::update::compile_update;
use sqlite_rs::codegen::{compile_delete, compile_insert};
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::parser::ast::Update;
use sqlite_rs::parser::{
    parse_delete, parse_insert, parse_update, DeleteOutcome, InsertOutcome, ParseOutcome,
};
use sqlite_rs::schema::{read_schema, TableSchema};
use sqlite_rs::vdbe::execute_with_writable_db;
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-index-maintenance-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn seed(oracle: &PathBuf, db: &PathBuf, sql: &str) {
    let status = Command::new(oracle).arg(db).arg(sql).status().unwrap();
    assert!(status.success());
}

fn page_size_of(db: &PathBuf) -> u32 {
    let vfs = UnixVfs;
    let file = vfs.open_read(db).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let page_size = u16::from_be_bytes([header_buf[16], header_buf[17]]) as u32;
    if page_size == 1 {
        65536
    } else {
        page_size
    }
}

fn read_header(db: &PathBuf, page_size: u32) -> DatabaseHeader {
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, db, page_size).unwrap();
    let raw = pager.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&raw[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

fn table_schema(db: &PathBuf, header: &DatabaseHeader, table: &str) -> TableSchema {
    let vfs = UnixVfs;
    let source = VfsPageSource::open(&vfs, db, header.page_size).unwrap();
    let mut cursor = TableCursor::new(source, header, 1);
    let schemas = read_schema(&mut cursor, header.text_encoding).unwrap();
    schemas
        .into_iter()
        .find(|s| s.name == table)
        .unwrap_or_else(|| panic!("no schema for table {table}"))
}

fn assert_integrity_ok(oracle: &PathBuf, db: &PathBuf) {
    let integrity = Command::new(oracle)
        .arg("-readonly")
        .arg(db)
        .arg("PRAGMA integrity_check;")
        .output()
        .unwrap();
    assert!(integrity.status.success());
    assert_eq!(String::from_utf8_lossy(&integrity.stdout).trim(), "ok");
}

fn oracle_select(oracle: &PathBuf, db: &PathBuf, sql: &str) -> String {
    let out = Command::new(oracle)
        .arg("-readonly")
        .arg("-list")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn insert_maintains_a_secondary_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("insert");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_b ON t(b); \
         INSERT INTO t VALUES (1, 'seed');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes.len(), 1);

    let insert = match parse_insert("INSERT INTO t VALUES (2, 'apple'), (3, 'banana')") {
        InsertOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_b WHERE b = 'apple'"
        ),
        "2"
    );
}

#[test]
fn delete_maintains_a_secondary_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("delete");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_b ON t(b); \
         INSERT INTO t VALUES (1, 'apple'), (2, 'banana'), (3, 'cherry');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let delete = match parse_delete("DELETE FROM t WHERE b = 'banana'") {
        DeleteOutcome::Accepted(d) => *d,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_delete(&delete, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT count(*) FROM t INDEXED BY idx_b WHERE b = 'banana'"
        ),
        "0"
    );
}

#[test]
fn update_maintains_a_secondary_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("update");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_b ON t(b); \
         INSERT INTO t VALUES (1, 'apple'), (2, 'banana');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let update: Update = match parse_update("UPDATE t SET b = 'zebra' WHERE id = 2") {
        ParseOutcome::Accepted(u) => *u,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_update(&update, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_b WHERE b = 'zebra'"
        ),
        "2"
    );
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT count(*) FROM t INDEXED BY idx_b WHERE b = 'banana'"
        ),
        "0"
    );
}

/// Reassigning the rowid-alias column moves the row to a new rowid at
/// the table-btree level; the *old* index entry (keyed on the old
/// rowid) must be deleted before the row moves, and the new entry
/// (keyed on the new rowid) inserted after — exercising the trickiest
/// ordering case `update.rs`'s own module doc calls out.
#[test]
fn update_reassigning_rowid_alias_maintains_the_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("update-rowid");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_b ON t(b); \
         INSERT INTO t VALUES (1, 'apple'), (2, 'banana');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let update: Update = match parse_update("UPDATE t SET id = 99 WHERE id = 2") {
        ParseOutcome::Accepted(u) => *u,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_update(&update, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_b WHERE b = 'banana'"
        ),
        "99"
    );
}

/// Multiple indexes on the same table: cursor numbering
/// (`open_index_cursors`/`emit_index_key_ops` both offset by index
/// position) must not collide, and every index must independently stay
/// in sync.
#[test]
fn insert_maintains_multiple_secondary_indexes() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("multi-index");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT, c TEXT); \
         CREATE INDEX idx_b ON t(b); \
         CREATE INDEX idx_c ON t(c); \
         INSERT INTO t VALUES (1, 'seed', 'seed');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert_eq!(schema.indexes.len(), 2);

    let insert = match parse_insert("INSERT INTO t VALUES (2, 'apple', 'red')") {
        InsertOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_b WHERE b = 'apple'"
        ),
        "2"
    );
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_c WHERE c = 'red'"
        ),
        "2"
    );
}

/// A multi-column index: register contiguity across more than one
/// index column, plus the trailing rowid slot, must hold.
#[test]
fn insert_maintains_a_multicolumn_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("multicolumn-index");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT, c TEXT); \
         CREATE INDEX idx_bc ON t(b, c); \
         INSERT INTO t VALUES (1, 'seed', 'seed');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let insert = match parse_insert("INSERT INTO t VALUES (2, 'apple', 'red')") {
        InsertOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_bc WHERE b = 'apple' AND c = 'red'"
        ),
        "2"
    );
}

/// An index on the rowid-alias column itself: `emit_column_read`'s
/// rowid substitution (the on-disk record stores `NULL` for that
/// column) must produce the real rowid value in the index key, not
/// `NULL`.
#[test]
fn insert_maintains_an_index_on_the_rowid_alias_column() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("rowid-alias-index");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_id ON t(id); \
         INSERT INTO t VALUES (1, 'seed');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let insert = match parse_insert("INSERT INTO t VALUES (2, 'apple')") {
        InsertOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT b FROM t INDEXED BY idx_id WHERE id = 2"
        ),
        "apple"
    );
}

/// A `DESC`-ordered index column: there is no b-tree comparator support
/// for descending index keys anywhere in this codebase yet (a
/// pre-existing #171 gap, not introduced here), so codegen must reject
/// it loudly (`CodegenError::Unsupported`) rather than silently build
/// an ascending key stock `sqlite3` would then compare backwards.
#[test]
fn insert_on_desc_index_is_rejected_not_silently_miskeyed() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("desc-index");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT); \
         CREATE INDEX idx_b_desc ON t(b DESC); \
         INSERT INTO t VALUES (1, 'seed');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert!(schema.indexes[0].columns[0].desc);

    let insert = match parse_insert("INSERT INTO t VALUES (2, 'apple')") {
        InsertOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let err = compile_insert(&insert, &schema).unwrap_err();
    assert!(
        matches!(err, sqlite_rs::codegen::CodegenError::Unsupported { .. }),
        "expected Unsupported, got {err:?}"
    );
}

/// `INSERT OR REPLACE` displacing an existing row must remove that
/// row's secondary-index entries before writing the replacement's —
/// `emit_pk_conflict`'s `Replace` arm predates index maintenance and
/// wasn't updated to call `emit_index_key_ops` when #196 landed,
/// leaving a stale index entry for the displaced row's old value.
#[test]
fn insert_or_replace_removes_the_displaced_rows_index_entry() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("or-replace");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT); \
         CREATE INDEX idx_v ON t(v); \
         INSERT INTO t VALUES (1, 'a');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let insert = match parse_insert("INSERT OR REPLACE INTO t VALUES (1, 'b')") {
        InsertOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT count(*) FROM t INDEXED BY idx_v WHERE v = 'a'"
        ),
        "0"
    );
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_v WHERE v = 'b'"
        ),
        "1"
    );
}

/// INSERT -> UPDATE -> DELETE through our own codegen, in sequence,
/// against the same indexed table — an integration scenario no
/// single-statement test exercises (each other test in this file seeds
/// the table via the oracle and runs exactly one of our codegen paths).
/// Checks `PRAGMA integrity_check` after every step.
#[test]
fn insert_update_delete_lifecycle_keeps_the_index_consistent() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("lifecycle");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT); \
         CREATE INDEX idx_v ON t(v);",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);

    let schema = table_schema(&db, &header, "t");
    let insert = match parse_insert("INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')") {
        InsertOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();
    assert_integrity_ok(&oracle, &db);

    let schema = table_schema(&db, &header, "t");
    let update: Update = match parse_update("UPDATE t SET v = 'z' WHERE id = 2") {
        ParseOutcome::Accepted(u) => *u,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_update(&update, &schema).unwrap();
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();
    assert_integrity_ok(&oracle, &db);

    let schema = table_schema(&db, &header, "t");
    let delete = match parse_delete("DELETE FROM t WHERE id = 1") {
        DeleteOutcome::Accepted(d) => *d,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_delete(&delete, &schema).unwrap();
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();
    assert_integrity_ok(&oracle, &db);

    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_v WHERE v = 'z'"
        ),
        "2"
    );
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT count(*) FROM t INDEXED BY idx_v WHERE v = 'a'"
        ),
        "0"
    );
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_v WHERE v = 'c'"
        ),
        "3"
    );
}

/// Pins today's documented (non-enforcing) behavior for a duplicate
/// value into a `UNIQUE` index: it does NOT surface as a constraint
/// violation, and it does not even fail — the b-tree's own duplicate
/// check (`btree::insert_entry`) compares the *whole* key, index
/// column(s) plus the trailing rowid, so two rows with the same
/// `v` but different rowids never collide at the b-tree level. UNIQUE
/// enforcement is tracked separately (#207) — but without a test, a
/// future reader could easily believe UNIQUE indexes already reject
/// duplicates when they don't.
#[test]
fn insert_duplicate_key_into_unique_index_is_not_enforced_as_a_constraint() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("unique-index");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT); \
         CREATE UNIQUE INDEX idx_v ON t(v); \
         INSERT INTO t VALUES (1, 'a');",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");
    assert!(schema.indexes[0].unique);

    let insert = match parse_insert("INSERT INTO t VALUES (2, 'a')") {
        InsertOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header)
        .expect("today's (incorrect) behavior: a duplicate UNIQUE value is not rejected (#207)");

    // The b-tree's own duplicate check compares the whole key (index
    // column(s) + trailing rowid, see `btree::insert_entry`), so two
    // rows with the same `v` but different rowids never collide there
    // — both rows land in the table (not rejected) and the *declared*
    // UNIQUE index ends up with two entries for the same value, which
    // `PRAGMA integrity_check` — not a plain `SELECT`, since stock
    // sqlite3's query planner assumes a UNIQUE index has at most one
    // match and can short-circuit an `INDEXED BY` equality lookup after
    // the first hit — correctly flags as `non-unique entry in index`.
    assert_eq!(
        oracle_select(&oracle, &db, "SELECT count(*) FROM t"),
        "2",
        "both rows are written; the duplicate is not rejected"
    );
    let integrity = Command::new(&oracle)
        .arg("-readonly")
        .arg(&db)
        .arg("PRAGMA integrity_check;")
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&integrity.stdout).contains("non-unique entry"),
        "expected integrity_check to flag the UNIQUE index as violated, got: {}",
        String::from_utf8_lossy(&integrity.stdout)
    );
}

/// An `AUTOINCREMENT` table with a secondary index: rowids assigned via
/// `sqlite_sequence` bookkeeping (#193) must still produce correct,
/// consistent index keys once #196's `NewRowid`/index-maintenance
/// wiring is involved.
#[test]
fn insert_into_autoincrement_table_maintains_its_index() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("index_maintenance");
        return;
    };
    let db = scratch_db("autoincrement-index");
    seed(
        &oracle,
        &db,
        "CREATE TABLE t(id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT); \
         CREATE INDEX idx_v ON t(v); \
         INSERT INTO t(v) VALUES ('a'); \
         DELETE FROM t;",
    );

    let page_size = page_size_of(&db);
    let header = read_header(&db, page_size);
    let schema = table_schema(&db, &header, "t");

    let insert = match parse_insert("INSERT INTO t(v) VALUES ('b')") {
        InsertOutcome::Accepted(i) => *i,
        other => panic!("failed to parse: {other:?}"),
    };
    let program = compile_insert(&insert, &schema).unwrap();
    let vfs = UnixVfs;
    let pager = Pager::open(&vfs, &db, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    assert_integrity_ok(&oracle, &db);
    // AUTOINCREMENT never reuses a rowid even after the table was
    // emptied — the new row must be keyed (in both the table and the
    // index) on a rowid greater than the deleted one, not `1` again.
    assert_eq!(
        oracle_select(
            &oracle,
            &db,
            "SELECT id FROM t INDEXED BY idx_v WHERE v = 'b'"
        ),
        "2"
    );
}
