// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! Spike #8 / 005_locking_interop: does a Rust process taking byte-identical
//! `fcntl` locks actually interop with a live, stock `sqlite3` process?
//!
//! Five throwaway two-process experiments, run against a real `sqlite3`
//! binary on PATH. See findings.md for results and surprises.

mod harness;
mod lock;
mod wal_shm;

use lock::LockAttempt;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::Duration;

struct Outcome {
    name: &'static str,
    passed: bool,
    detail: String,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "--probe" {
        run_probe(&args[2], &args[3]);
        return;
    }

    let dir = std::env::temp_dir().join(format!("spike005-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir scratch dir");

    let results = vec![
        exp1_reader_blocks_writer(&dir),
        exp2_writer_blocks_reader(&dir),
        exp3_pending_semantics(&dir),
        exp4_wal_read_lock_vs_checkpointer(&dir),
        exp5_close_drops_locks(&dir),
    ];

    println!();
    let mut all_passed = true;
    for r in &results {
        let tag = if r.passed { "PASS" } else { "FAIL" };
        if !r.passed {
            all_passed = false;
        }
        println!("[{tag}] {} — {}", r.name, r.detail);
    }
    println!();
    std::fs::remove_dir_all(&dir).ok();
    std::process::exit(if all_passed { 0 } else { 1 });
}

/// Dispatch for the re-exec'd probe subprocess (a genuine second OS process).
fn run_probe(mode: &str, path: &str) {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("probe open failed");
    let fd = file.as_raw_fd();
    let result = match mode {
        "pending" => lock::try_lock(fd, libc::F_RDLCK as libc::c_int, lock::PENDING_BYTE, 1),
        "shared" => lock::try_lock(
            fd,
            libc::F_RDLCK as libc::c_int,
            lock::SHARED_FIRST,
            lock::SHARED_SIZE,
        ),
        "reserved_demo_a" => {
            lock::try_lock(fd, libc::F_WRLCK as libc::c_int, lock::SHARED_FIRST + 5, 1)
        }
        "reserved_demo_b" => {
            lock::try_lock(fd, libc::F_WRLCK as libc::c_int, lock::SHARED_FIRST + 10, 1)
        }
        other => panic!("unknown probe mode {other:?}"),
    }
    .expect("probe lock syscall failed");
    println!(
        "{}",
        if result == LockAttempt::Acquired {
            "ACQUIRED"
        } else {
            "BLOCKED"
        }
    );
}

fn fresh_db(dir: &Path, name: &str) -> String {
    let db = dir.join(name);
    let db_str = db.to_str().unwrap().to_string();
    let (ok, _out, err) = harness::run_sql(&db_str, "CREATE TABLE t(x); INSERT INTO t VALUES(1);");
    assert!(ok, "fixture setup failed: {err}");
    db_str
}

fn exp1_reader_blocks_writer(dir: &Path) -> Outcome {
    let db = fresh_db(dir, "exp1.db");
    let file = OpenOptions::new().read(true).write(true).open(&db).unwrap();
    let fd = file.as_raw_fd();

    let held = lock::try_lock(
        fd,
        libc::F_RDLCK as libc::c_int,
        lock::SHARED_FIRST,
        lock::SHARED_SIZE,
    )
    .unwrap();
    let (ok, _out, err) = harness::run_sql(&db, "PRAGMA busy_timeout=0; INSERT INTO t VALUES(2);");
    lock::unlock(fd, lock::SHARED_FIRST, lock::SHARED_SIZE).unwrap();

    let passed = held == LockAttempt::Acquired && !ok && err.to_lowercase().contains("locked");
    Outcome {
        name: "1. reader (our SHARED lock) blocks stock sqlite3 writer",
        passed,
        detail: format!(
            "our lock={held:?}, sqlite3 insert ok={ok}, stderr={:?}",
            err.trim()
        ),
    }
}

fn exp2_writer_blocks_reader(dir: &Path) -> Outcome {
    let db = fresh_db(dir, "exp2.db");
    let mut session = harness::Session::spawn(&db);
    session.send_and_sync("BEGIN EXCLUSIVE;", "excl-held");

    let file = OpenOptions::new().read(true).write(true).open(&db).unwrap();
    let fd = file.as_raw_fd();
    let attempt = lock::try_lock(
        fd,
        libc::F_RDLCK as libc::c_int,
        lock::SHARED_FIRST,
        lock::SHARED_SIZE,
    )
    .unwrap();

    session.send("COMMIT;");
    session.wait();

    let passed = attempt == LockAttempt::Blocked;
    Outcome {
        name: "2. stock sqlite3 EXCLUSIVE blocks our SHARED read attempt",
        passed,
        detail: format!("our shared-lock attempt while sqlite3 held EXCLUSIVE: {attempt:?}"),
    }
}

fn exp3_pending_semantics(dir: &Path) -> Outcome {
    let db = fresh_db(dir, "exp3.db");
    let file = OpenOptions::new().read(true).write(true).open(&db).unwrap();
    let fd = file.as_raw_fd();

    let held = lock::try_lock(
        fd,
        libc::F_RDLCK as libc::c_int,
        lock::SHARED_FIRST,
        lock::SHARED_SIZE,
    )
    .unwrap();

    let mut session = harness::Session::spawn(&db);
    session.send_and_sync("PRAGMA busy_timeout=3000;", "bt-set");
    session.send_and_sync("BEGIN IMMEDIATE;", "reserved-held");
    // COMMIT now tries to escalate RESERVED -> EXCLUSIVE. It grabs PENDING_BYTE
    // immediately, then retries the full-EXCLUSIVE range for up to busy_timeout
    // because our SHARED lock is still held. We can't sentinel-sync past this
    // (it's stuck retrying), so give it a moment to enter the retry loop.
    session.send("COMMIT;");
    std::thread::sleep(Duration::from_millis(200));

    let new_reader = harness::probe_in_subprocess("pending", &db);

    lock::unlock(fd, lock::SHARED_FIRST, lock::SHARED_SIZE).unwrap();
    let status = session.wait();

    let passed = held == LockAttempt::Acquired && new_reader == "BLOCKED" && status.success();
    Outcome {
        name: "3. PENDING (held by stock sqlite3) refuses a brand-new SHARED reader",
        passed,
        detail: format!(
            "our shared-lock={held:?}, new-reader probe while sqlite3 mid-COMMIT retry={new_reader}, sqlite3 exit ok={}",
            status.success()
        ),
    }
}

fn checkpoint_truncate(db: &str) -> (bool, u32, u32, u32) {
    let (ok, out, err) = harness::run_sql(db, "PRAGMA wal_checkpoint(TRUNCATE);");
    assert!(ok, "checkpoint pragma failed: {err}");
    let fields: Vec<u32> = out
        .trim()
        .split('|')
        .map(|s| s.parse().unwrap_or(u32::MAX))
        .collect();
    (ok, fields[0], fields[1], fields[2])
}

fn exp4_wal_read_lock_vs_checkpointer(dir: &Path) -> Outcome {
    let db = dir.join("exp4.db");
    let db_str = db.to_str().unwrap().to_string();
    let shm_str = format!("{db_str}-shm");
    let wal_str = format!("{db_str}-wal");

    // A persistent writer session, kept open across the whole experiment.
    // One-shot `sqlite3 db "..."` calls each open+close a connection, and
    // closing the last connection fully checkpoints (and can reset) the WAL
    // regardless of our lock — a harness confound, not a locking result
    // (matches 004_wal_reading's own finding: close-time auto-checkpoint).
    // A real writer app keeps its connection open, so this is also the more
    // realistic shape for this experiment.
    let mut writer = harness::Session::spawn(&db_str);
    writer.send("PRAGMA journal_mode=WAL;");
    writer.send("CREATE TABLE t(x);");
    writer.send_and_sync("INSERT INTO t VALUES(1);", "seed-done");

    let shm = wal_shm::ShmMap::open(&shm_str);
    let mx_frame = shm.mx_frame();
    let slot = (1..=4usize)
        .find(|&s| shm.read_mark(s) == wal_shm::READ_MARK_UNUSED)
        .unwrap_or(1);

    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&shm_str)
        .unwrap();
    let lock_fd = lock_file.as_raw_fd();
    let byte = lock::wal_read_lock_byte(slot as i64);

    lock::try_lock(lock_fd, libc::F_WRLCK as libc::c_int, byte, 1).unwrap();
    shm.set_read_mark(slot, mx_frame);
    let held = lock::try_lock(lock_fd, libc::F_RDLCK as libc::c_int, byte, 1).unwrap();

    writer.send_and_sync(
        "INSERT INTO t VALUES(2); INSERT INTO t VALUES(3);",
        "more-done",
    );
    let mx_frame_after_insert = shm.mx_frame();
    let wal_size_before = std::fs::metadata(&wal_str).map(|m| m.len()).unwrap_or(0);

    let (_, busy1, _log1, _ckpt1) = checkpoint_truncate(&db_str);
    let wal_size_after_blocked = std::fs::metadata(&wal_str).map(|m| m.len()).unwrap_or(0);

    lock::unlock(lock_fd, byte, 1).unwrap();
    let (_, busy2, _log2, _ckpt2) = checkpoint_truncate(&db_str);
    let wal_size_after_released = std::fs::metadata(&wal_str).map(|m| m.len()).unwrap_or(0);

    writer.wait();

    eprintln!(
        "[exp4] claimed mxFrame={mx_frame} at slot={slot} (byte={byte}); \
         after 2 more inserts on the SAME still-open writer conn: mxFrame={mx_frame_after_insert}, wal_size={wal_size_before}; \
         checkpoint(TRUNCATE) while our lock held: busy={busy1}, wal_size={wal_size_after_blocked}; \
         checkpoint(TRUNCATE) after release: busy={busy2}, wal_size={wal_size_after_released}"
    );

    let blocked_ok = busy1 == 1 || wal_size_after_blocked > 0;
    let released_ok = busy2 == 0 && wal_size_after_released == 0;
    let passed = held == LockAttempt::Acquired && blocked_ok && released_ok;

    Outcome {
        name: "4. our WAL read-lock slot + mark makes sqlite3's checkpointer back off",
        passed,
        detail: format!(
            "mark held={held:?}, mxFrame claimed={mx_frame}, slot={slot}, \
             wal_size before_ckpt={wal_size_before} while_blocked(busy={busy1})={wal_size_after_blocked}, \
             after_release(busy={busy2})={wal_size_after_released}"
        ),
    }
}

fn exp5_close_drops_locks(dir: &Path) -> Outcome {
    let db = fresh_db(dir, "exp5.db");

    // Scenario A: the naive trap. fd_a locks; an unrelated fd_b to the SAME
    // file is opened and closed; per POSIX, close() on ANY fd for this
    // inode drops ALL of this process's fcntl locks on it, including fd_a's.
    let fd_a = OpenOptions::new().read(true).write(true).open(&db).unwrap();
    let a_held = lock::try_lock(
        fd_a.as_raw_fd(),
        libc::F_WRLCK as libc::c_int,
        lock::SHARED_FIRST + 5,
        1,
    )
    .unwrap();
    {
        let fd_b = OpenOptions::new().read(true).write(true).open(&db).unwrap();
        drop(fd_b); // closes a second, unrelated fd on the same inode
    }
    let trap_probe = harness::probe_in_subprocess("reserved_demo_a", &db);

    // Scenario B: the fd-cache-shaped workaround. A second logical need for
    // this inode reuses fd_a's own handle (no second real open()); "closing"
    // that logical reference is a no-op on the real fd, so the lock survives.
    let a2_held = lock::try_lock(
        fd_a.as_raw_fd(),
        libc::F_WRLCK as libc::c_int,
        lock::SHARED_FIRST + 10,
        1,
    )
    .unwrap();
    let logical_ref_reused_fd_a = fd_a.as_raw_fd(); // stand-in for a cache hit, not a new open()
    let _ = logical_ref_reused_fd_a; // "released" without ever calling close(2)
    let workaround_probe = harness::probe_in_subprocess("reserved_demo_b", &db);

    let passed = a_held == LockAttempt::Acquired
        && trap_probe == "ACQUIRED" // lock silently gone -> trap reproduced
        && a2_held == LockAttempt::Acquired
        && workaround_probe == "BLOCKED"; // lock survived -> workaround shape holds

    Outcome {
        name: "5. close()-drops-all-locks trap, and the fd-cache-shaped workaround",
        passed,
        detail: format!(
            "trap: our lock={a_held:?}, external probe after unrelated close()={trap_probe} \
             (ACQUIRED=trap reproduced); workaround: our lock={a2_held:?}, external probe \
             without a real close()={workaround_probe} (BLOCKED=lock survived)"
        ),
    }
}
