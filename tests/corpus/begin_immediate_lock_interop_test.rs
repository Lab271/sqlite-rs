//! #395 acceptance proof: a compiled SQL-level `BEGIN IMMEDIATE`/`BEGIN
//! EXCLUSIVE` must visibly block a concurrent stock `sqlite3` writer at
//! `BEGIN` time, not just at `COMMIT` — the same guarantee
//! `lock_state_interop_test.rs` proves for the raw `FileLockState`
//! primitive, exercised here through `compile_begin`/`Pager::flush`
//! instead.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use std::cell::RefCell;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_statement;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::execute_transaction_step;
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-begin-immediate-interop-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

fn oracle_exec(oracle: &Path, db: &Path, sql: &str) {
    let status = Command::new(oracle).arg(db).arg(sql).status().unwrap();
    assert!(status.success(), "oracle script failed: {sql}");
}

fn header_of(vfs: &UnixVfs, db: &Path, page_size: u32) -> DatabaseHeader {
    let source = Pager::open(vfs, db, page_size).unwrap();
    let bytes = source.read_page(1).unwrap();
    let mut buf = [0u8; 100];
    buf.copy_from_slice(&bytes[..100]);
    DatabaseHeader::parse(&buf).unwrap()
}

/// A live multi-statement session against our own engine (mirroring
/// `transaction_oracle_test.rs`'s `run_our_session`), except statements
/// run one at a time so a test can pause mid-transaction — right after
/// `BEGIN IMMEDIATE`/`BEGIN EXCLUSIVE` — and probe lock contention with a
/// concurrent stock `sqlite3` process before continuing to `COMMIT`.
struct OurSession {
    pager: Rc<RefCell<Pager>>,
    header: DatabaseHeader,
    schemas: Vec<sqlite_rs::schema::TableSchema>,
    autocommit: bool,
}

impl OurSession {
    fn open(vfs: &UnixVfs, db: &Path, page_size: u32) -> Self {
        let header = header_of(vfs, db, page_size);
        let pager = Rc::new(RefCell::new(Pager::open(vfs, db, page_size).unwrap()));
        let schemas = {
            let borrowed = pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
            read_schema(&mut schema_cursor, header.text_encoding).unwrap()
        };
        OurSession {
            pager,
            header,
            schemas,
            autocommit: true,
        }
    }

    fn exec(&mut self, stmt: &str) {
        let program = compile_statement(stmt, &self.schemas).unwrap();
        let (_, autocommit) = execute_transaction_step(
            &program,
            Rc::clone(&self.pager),
            self.header,
            self.autocommit,
        )
        .unwrap();
        self.autocommit = autocommit;
    }
}

#[test]
fn begin_immediate_blocks_concurrent_stock_sqlite3_write() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("begin_immediate_blocks_concurrent_stock_sqlite3_write");
        return;
    };

    let db = scratch_db("begin-immediate");
    oracle_exec(
        &oracle,
        &db,
        "create table t(a integer); insert into t values (1);",
    );

    let vfs = UnixVfs;
    let page_size = header_of(&vfs, &db, 4096).page_size;

    let mut session = OurSession::open(&vfs, &db, page_size);
    session.exec("BEGIN IMMEDIATE");

    // RESERVED is now held from `BEGIN IMMEDIATE` itself, before any
    // write — a stock sqlite3 writer given no time to wait (default
    // busy_timeout is 0) must fail immediately with "database is locked".
    let output = Command::new(&oracle)
        .arg(&db)
        .arg("insert into t values (2);")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a concurrent stock sqlite3 write must be blocked by our BEGIN IMMEDIATE"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("locked"),
        "expected a 'database is locked' error, got: {:?}",
        output
    );

    session.exec("COMMIT");
    // `Pager::open` holds a plain SHARED lock for its whole lifetime
    // (released only on `Drop`) — harmless for a concurrent *read*, but a
    // concurrent *write* still needs a brief EXCLUSIVE step to commit, so
    // the session must actually close before that can succeed, exactly
    // like a stock sqlite3 connection going idle.
    drop(session);

    // The lock is released once we commit (even though nothing was
    // written on our side), so the same write must now succeed.
    let status = Command::new(&oracle)
        .arg(&db)
        .arg("insert into t values (2);")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "sqlite3 write must succeed once our BEGIN IMMEDIATE's lock is released by COMMIT"
    );

    std::fs::remove_dir_all(db.parent().unwrap()).ok();
}

#[test]
fn begin_exclusive_blocks_concurrent_stock_sqlite3_read() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("begin_exclusive_blocks_concurrent_stock_sqlite3_read");
        return;
    };

    let db = scratch_db("begin-exclusive");
    oracle_exec(
        &oracle,
        &db,
        "create table t(a integer); insert into t values (1);",
    );

    let vfs = UnixVfs;
    let page_size = header_of(&vfs, &db, 4096).page_size;

    let mut session = OurSession::open(&vfs, &db, page_size);
    session.exec("BEGIN EXCLUSIVE");

    // EXCLUSIVE blocks every other lock level, including a plain SHARED
    // read — a concurrent stock sqlite3 `SELECT` must fail immediately.
    let output = Command::new(&oracle)
        .arg(&db)
        .arg("select * from t;")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a concurrent stock sqlite3 read must be blocked by our BEGIN EXCLUSIVE"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("locked"),
        "expected a 'database is locked' error, got: {:?}",
        output
    );

    session.exec("ROLLBACK");

    let status = Command::new(&oracle)
        .arg(&db)
        .arg("select * from t;")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "sqlite3 read must succeed once our BEGIN EXCLUSIVE's lock is released by ROLLBACK"
    );

    std::fs::remove_dir_all(db.parent().unwrap()).ok();
}
