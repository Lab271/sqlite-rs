// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! #706: two `Connection`s (here, two independent `Pager`/`Vm` sessions
//! built directly over `UnixVfs`, without going through `src/api.rs` —
//! PR #705 isn't merged yet) opened on the same file **in the same
//! process** did not lock against each other. `BEGIN IMMEDIATE` on one
//! handle escalated only *its own* `FileLockState`'s fcntl lock; a second
//! handle's own, independently-opened fd on the same inode took the
//! conflicting fcntl lock too, since POSIX `fcntl` never conflicts with a
//! lock already held by the calling process — so the second handle's
//! write silently succeeded and the first handle's commit clobbered it on
//! disk.
//!
//! `src/vfs/inode_registry.rs` fixes this by sharing one process-wide
//! `SharedInodeLock` per `(device, inode)`, arbitrating in-process
//! requests before any real `fcntl` call. This file proves the fix at the
//! same altitude #491/#412 were investigating: real `Pager`s, real
//! `BEGIN IMMEDIATE`, no mocked locks. Schema setup goes through the
//! pinned oracle (like `begin_immediate_lock_interop_test.rs`), so every
//! test here is oracle-gated even though the reproduction itself is
//! entirely in-process.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::codegen::compile_statement;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::schema::read_schema;
use sqlite_rs::vdbe::{execute_transaction_step, ExecError};
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::oracle::{assert_integrity_check_ok, oracle_list_output, pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-in-process-lock-registry-{label}-{}-{n}",
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

/// A minimal in-process session: its own `Pager` opened independently
/// against the same path another session may already have open — the
/// exact "two `Connection`s, one file, one process" shape #706 named.
/// Schema is read fresh from disk at construction, same as
/// `begin_immediate_lock_interop_test.rs`'s `OurSession`; unlike that
/// type, `exec` here returns `Result` instead of unwrapping, so a test
/// can assert on the second handle's write being refused rather than
/// treat it as a panic-worthy bug.
struct Session {
    pager: Rc<RefCell<Pager>>,
    header: DatabaseHeader,
    schemas: Vec<sqlite_rs::schema::TableSchema>,
    autocommit: bool,
}

impl Session {
    fn open(vfs: &UnixVfs, db: &Path, page_size: u32) -> Self {
        let header = header_of(vfs, db, page_size);
        let pager = Rc::new(RefCell::new(Pager::open(vfs, db, page_size).unwrap()));
        let schemas = {
            let borrowed = pager.borrow();
            let mut schema_cursor = TableCursor::new(&*borrowed, &header, 1);
            read_schema(&mut schema_cursor, header.text_encoding).unwrap()
        };
        Session {
            pager,
            header,
            schemas,
            autocommit: true,
        }
    }

    fn exec(&mut self, stmt: &str) -> Result<(), ExecError> {
        let program = compile_statement(stmt, &self.schemas, &[]).unwrap();
        let (_, autocommit) = execute_transaction_step(
            &program,
            Rc::clone(&self.pager),
            self.header,
            self.autocommit,
        )?;
        self.autocommit = autocommit;
        Ok(())
    }
}

/// Row count of table `t`, via `dump_database` (fresh `Pager::open`
/// each call, so it always reads current on-disk state rather than a
/// stale cached schema/header).
fn row_count(vfs: &UnixVfs, db: &Path) -> usize {
    let result = sqlite_rs::dump::dump_database(vfs, db).unwrap();
    let table = result
        .tables
        .iter()
        .find(|t| t.name == "t")
        .expect("table t not found");
    table.rows.len()
}

/// The issue's exact reproduction: A `BEGIN IMMEDIATE` + insert; B's
/// insert must be refused (blocked/busy), not silently accepted and then
/// discarded by A's commit. After A commits, the file holds both rows and
/// `integrity_check` passes.
#[test]
fn two_in_process_connections_on_one_file_serialize_a_write() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("two_in_process_connections_on_one_file_serialize_a_write");
        return;
    };

    let db = scratch_db("two-connections");
    oracle_exec(
        &oracle,
        &db,
        "create table t(a integer); insert into t values (1);",
    );

    let vfs = UnixVfs;
    let mut session_a = Session::open(&vfs, &db, 4096);
    session_a.exec("BEGIN IMMEDIATE").unwrap();
    session_a.exec("INSERT INTO t VALUES (2)").unwrap();

    // Session B is a second, independent `Pager` on the very same path,
    // in this very same process — the exact shape that used to lose
    // data silently.
    let mut session_b = Session::open(&vfs, &db, 4096);
    let b_result = session_b.exec("INSERT INTO t VALUES (3)");
    assert!(
        b_result.is_err(),
        "a second in-process connection's write must not succeed while the \
         first holds BEGIN IMMEDIATE — got Ok, meaning it was silently \
         accepted and would be discarded on A's commit"
    );

    // B releases its handle (as a pool would return a failed borrow)
    // before A commits — a still-open reader would legitimately block A's
    // own EXCLUSIVE commit escalation in rollback-journal mode (real
    // stock sqlite3 behavior, not something #706 changes); this test is
    // about B's *write* being refused, not about reader/writer commit
    // ordering.
    drop(session_b);
    session_a.exec("COMMIT").unwrap();
    drop(session_a);

    assert_integrity_check_ok(&oracle, &db);
    let rows = oracle_list_output(&oracle, &db, "t", &["a".to_string()]);
    assert_eq!(rows.trim(), "1\n2", "row 3 must never have been written");
    assert_eq!(row_count(&vfs, &db), 2);
}

/// Regression guard: the same sequence, but B is a real second *process*
/// (stock `sqlite3`) rather than a second in-process handle — this must
/// keep behaving exactly as it did before #706 (cross-process locking was
/// already correct; the fix must not break it while closing the
/// in-process gap).
#[test]
fn cross_process_locking_still_works_after_the_in_process_fix() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("cross_process_locking_still_works_after_the_in_process_fix");
        return;
    };

    let db = scratch_db("cross-process-regression");
    oracle_exec(
        &oracle,
        &db,
        "create table t(a integer); insert into t values (1);",
    );

    let vfs = UnixVfs;
    let mut session_a = Session::open(&vfs, &db, 4096);
    session_a.exec("BEGIN IMMEDIATE").unwrap();
    session_a.exec("INSERT INTO t VALUES (2)").unwrap();

    let output = Command::new(&oracle)
        .arg(&db)
        .arg("insert into t values (3);")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a concurrent stock sqlite3 write must still be blocked by our BEGIN IMMEDIATE"
    );

    session_a.exec("COMMIT").unwrap();
    drop(session_a);

    let status = Command::new(&oracle)
        .arg(&db)
        .arg("insert into t values (3);")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "sqlite3 write must succeed once our BEGIN IMMEDIATE's lock is released"
    );

    assert_integrity_check_ok(&oracle, &db);
}

/// Lifecycle: closing every in-process handle on a file, then reopening
/// it, must not leave the registry wedged (stale lock state, or a
/// leaked-forever entry) — the fresh handle gets a fully-`Unlocked`
/// ladder and a plain write succeeds.
#[test]
fn registry_entry_does_not_survive_past_its_last_handle() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("registry_entry_does_not_survive_past_its_last_handle");
        return;
    };

    let db = scratch_db("lifecycle");
    oracle_exec(
        &oracle,
        &db,
        "create table t(a integer); insert into t values (1);",
    );

    let vfs = UnixVfs;
    {
        let mut session = Session::open(&vfs, &db, 4096);
        session.exec("BEGIN IMMEDIATE").unwrap();
        session.exec("INSERT INTO t VALUES (2)").unwrap();
        session.exec("COMMIT").unwrap();
        // `session` drops here — every handle on this inode is now gone.
    }

    // A brand-new handle must see a clean, unlocked ladder: BEGIN
    // IMMEDIATE succeeds immediately rather than reporting stale
    // contention from the handle that just closed.
    let mut reopened = Session::open(&vfs, &db, 4096);
    reopened.exec("BEGIN IMMEDIATE").unwrap();
    reopened.exec("INSERT INTO t VALUES (3)").unwrap();
    reopened.exec("COMMIT").unwrap();
    drop(reopened);

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(row_count(&vfs, &db), 3);
}

/// A pool of N in-process handles on one file must make progress: each
/// one opens, writes serially (waiting its turn is out of scope — this
/// crate's locks are non-blocking `F_SETLK`, so a caller retries rather
/// than the lock itself blocking), and none of them deadlocks or wedges
/// the registry for the next.
#[test]
fn a_pool_of_in_process_handles_makes_progress() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("a_pool_of_in_process_handles_makes_progress");
        return;
    };

    let db = scratch_db("pool-progress");
    oracle_exec(&oracle, &db, "create table t(a integer);");

    let vfs = UnixVfs;
    for i in 0..5 {
        let mut session = Session::open(&vfs, &db, 4096);
        session.exec("BEGIN IMMEDIATE").unwrap();
        session
            .exec(&format!("INSERT INTO t VALUES ({i})"))
            .unwrap();
        session.exec("COMMIT").unwrap();
    }

    assert_integrity_check_ok(&oracle, &db);
    assert_eq!(row_count(&vfs, &db), 5);
}
