// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Retryable-busy and durability against a real second process (spec 013
//! Requirement 5).
//!
//! These live here rather than in `tests/unit/api_durability_test.rs`
//! because they need a *separate process* to hold the lock. Two connections
//! in one process do not exclude each other — POSIX `fcntl` locks are
//! scoped to `(process, inode)` and this crate has no
//! `unixInodeInfo`-equivalent registry — which is recorded, measured and
//! ratcheted in that unit module. The pinned `sqlite3` is the second party
//! here, which also makes the claim stronger: the lock protocol is being
//! honoured against stock SQLite, not merely against ourselves.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use sqlite_rs::api::{Connection, Error};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-api-dur-oracle-{}-{label}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn prepared(dir: &Path) -> PathBuf {
    let db = dir.join("t.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES (1, 'x');")
        .unwrap();
    db
}

/// A separate `sqlite3` process holding a write transaction open.
///
/// It begins `IMMEDIATE` (so RESERVED is taken at `BEGIN` rather than at
/// the first write) and then waits on stdin, which keeps the lock held for
/// as long as this handle lives.
struct LockHolder {
    child: Child,
}

impl LockHolder {
    fn new(oracle: &Path, db: &Path) -> Self {
        let mut child = Command::new(oracle)
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("could not spawn the oracle as a lock holder");
        {
            let stdin = child.stdin.as_mut().expect("piped stdin");
            stdin
                .write_all(
                    b"BEGIN IMMEDIATE;\nINSERT INTO t VALUES (2, 'held');\nSELECT 'holding';\n",
                )
                .expect("could not start the holder's transaction");
            stdin.flush().expect("could not flush to the holder");
        }
        // Give it time to actually take the lock before anyone tests for
        // contention. A sleep rather than a poll because the thing to wait
        // for *is* the lock, and probing for it is what the callers do.
        std::thread::sleep(Duration::from_millis(300));
        Self { child }
    }

    /// Commits and exits, releasing the lock.
    fn release(mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            stdin.write_all(b"COMMIT;\n.quit\n").ok();
            stdin.flush().ok();
        }
        self.child.wait().ok();
        // Already waited; stop `Drop` from killing a reaped child.
        std::mem::forget(self);
    }
}

impl Drop for LockHolder {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn count(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM t")
        .unwrap()
        .expect("count returns a row")
        .get(0)
        .unwrap()
}

/// Requirement 5's third scenario.
#[test]
fn busy_is_retryable() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("busy_is_retryable");
        return;
    };
    let dir = scratch_dir("busy");
    let db = prepared(&dir);

    let holder = LockHolder::new(&oracle, &db);

    let conn = Connection::open(&db).unwrap();
    let err = conn
        .execute("INSERT INTO t VALUES (3, 'blocked')")
        .expect_err("another process holds the write lock");

    assert!(
        matches!(err, Error::Busy { .. }),
        "expected Error::Busy, got {err:?}"
    );
    assert!(err.is_retryable(), "a busy error must be retryable");
    assert_eq!(err.sqlite_code(), 5, "should report SQLITE_BUSY");

    holder.release();

    // The retry succeeds, and both writes are present.
    conn.execute("INSERT INTO t VALUES (3, 'retried')")
        .expect("the retry should succeed once the lock is released");
    assert_eq!(count(&conn), 3);

    std::fs::remove_dir_all(&dir).ok();
}

/// With a timeout set, a contended write waits it out rather than failing
/// immediately — and still reports `Busy` when the lock outlives the wait.
#[test]
fn a_busy_timeout_waits_before_giving_up() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("a_busy_timeout_waits_before_giving_up");
        return;
    };
    let dir = scratch_dir("wait");
    let db = prepared(&dir);

    let holder = LockHolder::new(&oracle, &db);

    let conn = Connection::open(&db).unwrap();
    conn.set_busy_timeout(Duration::from_millis(400)).unwrap();

    let start = Instant::now();
    let err = conn
        .execute("INSERT INTO t VALUES (3, 'blocked')")
        .expect_err("the lock is held for the whole timeout");
    let waited = start.elapsed();

    assert!(matches!(err, Error::Busy { .. }), "got {err:?}");
    assert!(
        waited >= Duration::from_millis(300),
        "should have waited out most of the 400ms timeout, waited only {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(10),
        "waited {waited:?}, far past the timeout — the deadline is not honoured"
    );

    drop(holder);
    std::fs::remove_dir_all(&dir).ok();
}

/// The retry loop picks the lock up when it is released mid-wait, so the
/// caller does not write a retry loop of its own — and the statement is
/// applied exactly once, not once per attempt.
///
/// The exactly-once half is the one that matters. In autocommit the
/// statement's mutations are already in the pager's dirty set when the
/// commit meets the lock, so a retry that did not roll back first would
/// insert the row again for every attempt.
#[test]
fn a_retried_statement_succeeds_exactly_once() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("a_retried_statement_succeeds_exactly_once");
        return;
    };
    let dir = scratch_dir("once");
    let db = prepared(&dir);

    let holder = LockHolder::new(&oracle, &db);

    let conn = Connection::open(&db).unwrap();
    conn.set_busy_timeout(Duration::from_secs(10)).unwrap();

    // Release the lock while the write below is retrying.
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        holder.release();
    });

    let start = Instant::now();
    let changed = conn
        .execute("INSERT INTO t VALUES (99, 'retried')")
        .expect("should succeed once the holder commits");
    let waited = start.elapsed();

    releaser.join().expect("the releasing thread panicked");

    assert_eq!(changed, 1, "the retried statement should report one row");
    assert!(
        waited >= Duration::from_millis(150),
        "should have actually waited for the lock, waited {waited:?}"
    );

    let applied: i64 = conn
        .query_row("SELECT count(*) FROM t WHERE a = 99")
        .unwrap()
        .unwrap()
        .get(0)
        .unwrap();
    assert_eq!(
        applied, 1,
        "the retried INSERT landed {applied} times, not once — the rollback \
         before retry is missing or ineffective"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Requirement 5's second scenario: a transaction committed under
/// `synchronous = FULL` survives the process being killed without
/// unwinding.
///
/// The child is this same test binary re-executed with
/// `SQLITE_RS_HARDKILL_DB` set: it commits, writes a marker file so the
/// parent knows the commit returned, and then blocks forever. The parent
/// SIGKILLs it (`Child::kill` is SIGKILL on Unix), so no destructor, no
/// deferred flush and no unwinding runs — the only thing that can make the
/// rows survive is the commit having completed before it returned.
///
/// What this does and does not prove, stated precisely because the
/// difference matters for a durability claim. It proves the commit was
/// complete and consistent *in the file* rather than buffered inside the
/// process, and that an abrupt death leaves nothing malformed. It does
/// **not** prove platter durability: SIGKILL does not clear the kernel's
/// page cache, so it cannot distinguish `synchronous = FULL` from `NORMAL`
/// or `OFF`. Only a power cut or a crash-injecting VFS separates those, and
/// `tests/corpus/crash_torture_test.rs` is where that regime lives.
/// `synchronous = FULL` is set here so this exercises the path a durable
/// consumer configures, not to claim the test verifies the fsync.
#[test]
fn commit_survives_hard_kill() {
    if let Ok(db) = std::env::var("SQLITE_RS_HARDKILL_DB") {
        // Diverges: the child blocks until it is killed.
        hard_kill_child(Path::new(&db));
    }
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("commit_survives_hard_kill");
        return;
    };
    let dir = scratch_dir("hardkill");
    let db = dir.join("kill.db");
    let marker = dir.join("committed");

    let exe = std::env::current_exe().expect("the test binary's own path");
    let mut child = Command::new(exe)
        .arg("--exact")
        .arg("api_durability_oracle_test::commit_survives_hard_kill")
        .arg("--nocapture")
        .env("SQLITE_RS_HARDKILL_DB", &db)
        .env("SQLITE_RS_HARDKILL_MARKER", &marker)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("could not re-execute this test binary as the child");

    // Wait for the child to report that its COMMIT returned.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() {
        if Instant::now() > deadline {
            child.kill().ok();
            child.wait().ok();
            panic!("the child never reported a completed commit");
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!("the child exited early with {status:?} instead of committing");
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // SIGKILL: no unwinding, no destructors, no deferred flush.
    child.kill().expect("could not kill the child");
    child.wait().ok();

    // The rows have to be there, read back by a fresh connection...
    let conn = Connection::open(&db).unwrap();
    let survivors: i64 = conn
        .query_row("SELECT count(*) FROM t")
        .unwrap()
        .expect("count returns a row")
        .get(0)
        .unwrap();
    assert_eq!(
        survivors, 50,
        "a transaction committed under synchronous=FULL did not survive SIGKILL"
    );
    drop(conn);

    // ...and the file has to be well-formed to stock sqlite3, not merely
    // readable by us. A hard kill mid-journal is exactly how a malformed
    // file happens.
    let output = Command::new(&oracle)
        .arg(&db)
        .arg("PRAGMA integrity_check;")
        .output()
        .expect("could not run the oracle");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "ok",
        "the killed process left the database malformed"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The child half of [`commit_survives_hard_kill`]: commit durably, say so,
/// then wait to be killed.
fn hard_kill_child(db: &Path) -> ! {
    let conn = Connection::open(db).expect("child could not open the database");
    conn.execute("CREATE TABLE t(a INTEGER, b TEXT)")
        .expect("child could not create the table");
    conn.pragma("synchronous", "FULL")
        .expect("child could not set synchronous=FULL");

    let tx = conn.transaction().expect("child could not begin");
    for i in 0..50 {
        tx.execute_with(
            "INSERT INTO t VALUES (?1, ?2)",
            vec![
                sqlite_rs::record::Value::from(i),
                sqlite_rs::record::Value::from("durable"),
            ],
        )
        .expect("child could not insert");
    }
    tx.commit().expect("child could not commit");

    // The commit has returned. Under synchronous=FULL that must mean the
    // bytes are on the platter.
    if let Ok(marker) = std::env::var("SQLITE_RS_HARDKILL_MARKER") {
        std::fs::write(marker, b"committed").expect("child could not write its marker");
    }

    // Wait to be killed. Nothing after this point may run.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
