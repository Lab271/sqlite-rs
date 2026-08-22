//! #357 cross-compat proof for the [`sqlite_rs::vfs::FileLockState`]
//! ladder against a live, stock `sqlite3` process — spike 005
//! (`tests/spike/005_locking_interop/findings.md`) validated the byte
//! offsets in isolation; these tests exercise the state machine itself in
//! both directions.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::vfs::{FileLockState, LockLevel};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-lock-state-interop-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

/// A stock `sqlite3` interactive session, driven over stdin, that has
/// executed `BEGIN EXCLUSIVE;` and is holding the resulting EXCLUSIVE
/// journal-mode lock until dropped.
struct OracleExclusiveSession {
    child: std::process::Child,
}

impl OracleExclusiveSession {
    fn start(oracle: &std::path::Path, db: &std::path::Path) -> Self {
        let mut child = Command::new(oracle)
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.as_mut().unwrap();
        // `.print` forces a flush we can read back, so we know the
        // EXCLUSIVE lock is actually held before proceeding.
        stdin
            .write_all(b"BEGIN EXCLUSIVE;\n.print locked\n")
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        assert_eq!(
            line.trim(),
            "locked",
            "sqlite3 failed to enter BEGIN EXCLUSIVE"
        );
        child.stdout = Some(reader.into_inner());
        OracleExclusiveSession { child }
    }

    fn release(mut self) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(b"COMMIT;\n.quit\n").ok();
        self.child.wait().unwrap();
    }
}

#[test]
fn our_shared_lock_detects_stock_sqlite3_exclusive() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("our_shared_lock_detects_stock_sqlite3_exclusive");
        return;
    };

    let db = scratch_db("reader-vs-oracle-writer");
    let status = Command::new(&oracle)
        .arg(&db)
        .arg("create table t(a integer);")
        .status()
        .unwrap();
    assert!(status.success());

    let session = OracleExclusiveSession::start(&oracle, &db);

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&db)
        .unwrap();
    let mut lock = FileLockState::new(file);
    let result = lock.set_level(LockLevel::Shared);
    assert!(
        result.is_err(),
        "a live stock sqlite3 holding EXCLUSIVE must block our SHARED attempt"
    );
    assert_eq!(lock.lock_state(), LockLevel::Unlocked);

    session.release();

    assert!(
        lock.set_level(LockLevel::Shared).is_ok(),
        "our SHARED attempt must succeed once sqlite3 releases EXCLUSIVE"
    );
}

#[test]
fn our_exclusive_lock_blocks_stock_sqlite3_write() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("our_exclusive_lock_blocks_stock_sqlite3_write");
        return;
    };

    let db = scratch_db("writer-vs-oracle-writer");
    let status = Command::new(&oracle)
        .arg(&db)
        .arg("create table t(a integer);")
        .status()
        .unwrap();
    assert!(status.success());

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&db)
        .unwrap();
    let mut lock = FileLockState::new(file);
    lock.set_level(LockLevel::Exclusive).unwrap();

    // A stock sqlite3 write, given no time to wait (default busy_timeout
    // is 0), must fail with "database is locked" while we hold EXCLUSIVE.
    let output = Command::new(&oracle)
        .arg(&db)
        .arg("insert into t values (1);")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("locked"),
        "expected a 'database is locked' error, got: {:?}",
        output
    );

    lock.set_level(LockLevel::Unlocked).unwrap();

    let status = Command::new(&oracle)
        .arg(&db)
        .arg("insert into t values (1);")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "sqlite3 write must succeed once our EXCLUSIVE lock is released"
    );
}
