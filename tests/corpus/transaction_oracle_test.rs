//! #360 acceptance proof: SQL-level `BEGIN`/`COMMIT`/`ROLLBACK` must
//! actually gate whether writes persist, matching stock `sqlite3`'s
//! behavior byte-for-byte at the row level. Before this ticket,
//! `Transaction`/`AutoCommit` were no-ops and every successful `Halt`
//! implicit-committed — `ROLLBACK` did nothing.
//!
//! `run_oracle` (`oracle.rs`) is deliberately read-only, so this file
//! shells out to the oracle binary directly for the writes (seeding the
//! fixture, and running the oracle's own `BEGIN ... ROLLBACK`/`COMMIT`
//! script to build the "expected" side of each comparison).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_statement;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::record::decode_record;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::execute_transaction_step;
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-txn-oracle-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn oracle_exec(oracle: &Path, db: &Path, sql: &str) {
    let status = Command::new(oracle).arg(db).arg(sql).status().unwrap();
    assert!(status.success(), "oracle script failed: {sql}");
}

fn oracle_select_a(oracle: &Path, db: &Path) -> String {
    let output = Command::new(oracle)
        .arg("-readonly")
        .arg("-list")
        .arg(db)
        .arg("select a from t;")
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Reads column `a` of every row of `t` through our own engine — a raw
/// b-tree scan, not a compiled `SELECT` (out of `compile_statement`'s
/// dispatch table), matching the pattern `src/bin/sqlite-rs/exec.rs`
/// itself uses to resolve a schema before compiling a statement.
fn our_select_a(vfs: &UnixVfs, db: &Path, page_size: u32) -> String {
    let pager = Pager::open(vfs, db, page_size).unwrap();
    let header = header_of(vfs, db, page_size);
    let schemas = {
        let mut schema_cursor = TableCursor::new(&pager, &header, 1);
        read_schema(&mut schema_cursor, header.text_encoding).unwrap()
    };
    let schema = schemas.iter().find(|s| s.name == "t").unwrap();
    let mut cursor = TableCursor::new(&pager, &header, schema.root_page);
    let mut values = Vec::new();
    let mut row = cursor.first_row().unwrap();
    while let Some(r) = row {
        let cols = decode_record(&r.payload, header.text_encoding).unwrap();
        let a = match &cols[0] {
            sqlite_rs::record::Value::Integer(i) => i.to_string(),
            other => panic!("expected column a to be an integer, got {other:?}"),
        };
        values.push(a);
        row = cursor.next_row().unwrap();
    }
    values.join("\n")
}

fn header_of(vfs: &UnixVfs, db: &Path, page_size: u32) -> DatabaseHeader {
    let source = Pager::open(vfs, db, page_size).unwrap();
    let bytes = source.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&bytes[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

/// Runs `stmts` (each a full SQL statement) through our engine on one
/// shared `Pager`/autocommit state — the multi-statement session
/// `execute_transaction_step` exists for (#360). Compiles each
/// statement against the schema as it stood when the *session* began,
/// same as `CREATE TABLE`/`INSERT` never happening mid-session here.
fn run_our_session(vfs: &UnixVfs, db: &Path, page_size: u32, stmts: &[&str]) {
    let header = header_of(vfs, db, page_size);
    let pager = Rc::new(RefCell::new(Pager::open(vfs, db, page_size).unwrap()));
    let schemas = {
        let borrowed = pager.borrow();
        let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
        read_schema(&mut schema_cursor, header.text_encoding).unwrap()
    };

    let mut autocommit = true;
    for stmt in stmts {
        let program = compile_statement(stmt, &schemas, &[]).unwrap();
        let (_, ac) =
            execute_transaction_step(&program, Rc::clone(&pager), header, autocommit).unwrap();
        autocommit = ac;
    }
}

#[test]
fn rollback_discards_writes_matching_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("rollback_discards_writes_matching_oracle");
        return;
    };

    let ours = scratch_db("rollback-ours");
    let theirs = scratch_db("rollback-theirs");
    let setup = "create table t(a integer); insert into t values (1);";
    oracle_exec(&oracle, &ours, setup);
    oracle_exec(&oracle, &theirs, setup);

    let vfs = UnixVfs;
    let page_size = header_of(&vfs, &ours, 4096).page_size;

    run_our_session(
        &vfs,
        &ours,
        page_size,
        &["BEGIN", "UPDATE t SET a = 99", "ROLLBACK"],
    );
    oracle_exec(&oracle, &theirs, "BEGIN; UPDATE t SET a = 99; ROLLBACK;");

    assert_eq!(our_select_a(&vfs, &ours, page_size), "1");
    assert_eq!(oracle_select_a(&oracle, &theirs), "1");
    assert_eq!(
        our_select_a(&vfs, &ours, page_size),
        oracle_select_a(&oracle, &theirs)
    );

    std::fs::remove_dir_all(ours.parent().unwrap()).ok();
    std::fs::remove_dir_all(theirs.parent().unwrap()).ok();
}

#[test]
fn commit_persists_writes_matching_oracle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("commit_persists_writes_matching_oracle");
        return;
    };

    let ours = scratch_db("commit-ours");
    let theirs = scratch_db("commit-theirs");
    let setup = "create table t(a integer); insert into t values (1);";
    oracle_exec(&oracle, &ours, setup);
    oracle_exec(&oracle, &theirs, setup);

    let vfs = UnixVfs;
    let page_size = header_of(&vfs, &ours, 4096).page_size;

    run_our_session(
        &vfs,
        &ours,
        page_size,
        &["BEGIN", "UPDATE t SET a = 99", "COMMIT"],
    );
    oracle_exec(&oracle, &theirs, "BEGIN; UPDATE t SET a = 99; COMMIT;");

    assert_eq!(our_select_a(&vfs, &ours, page_size), "99");
    assert_eq!(oracle_select_a(&oracle, &theirs), "99");
    assert_eq!(
        our_select_a(&vfs, &ours, page_size),
        oracle_select_a(&oracle, &theirs)
    );

    std::fs::remove_dir_all(ours.parent().unwrap()).ok();
    std::fs::remove_dir_all(theirs.parent().unwrap()).ok();
}
