//! #361 — V5's acceptance gate: repeated `kill -9` mid-write-loop must
//! never leave the database in a state a real `sqlite3` considers
//! corrupt. `write_loop_probe` (a genuine second OS process, like
//! `lock_probe` gives the lock tests) runs `BEGIN IMMEDIATE; INSERT;
//! UPDATE; COMMIT;` in a tight loop against a shared scratch db; this
//! test kills it at a random point, `n` times, and after every single
//! kill checks: opening the db recovers cleanly (no panic, no
//! `PagerError`), the `-journal` companion is gone once recovery has
//! run, and — via the pinned oracle — `PRAGMA integrity_check` still
//! says `ok`. Recovery itself (`Pager::open`) is the same hot-journal
//! path #172/#358/#359 already unit- and oracle-test in isolation;
//! this is the stochastic end-to-end proof that path is actually
//! sufficient under real, unpredictable kill timing, not just the
//! specific byte patterns those tests hand-construct.
//!
//! Iteration count defaults to 25 (not the issue's 100) to keep this
//! bearable inside `make test-corpus`'s regular run; override with
//! `SQLITE_RS_TORTURE_ITERATIONS` for a heavier manual pass (`env
//! SQLITE_RS_TORTURE_ITERATIONS=100 cargo test --test corpus
//! kill_9_mid_write_loop`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sqlite_rs::pager::Pager;
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs};

use crate::oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};

const PROBE: &str = env!("CARGO_BIN_EXE_write_loop_probe");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-crash-torture-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

fn iterations() -> u32 {
    std::env::var("SQLITE_RS_TORTURE_ITERATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25)
}

/// Deterministic pseudo-random 5-60ms spread — deliberately not an
/// RNG dependency for one test's sake; a fixed-but-varying sequence is
/// enough to land kills across many different points in the
/// probe's loop without the test's own randomness needing a seed.
fn delay_ms(i: u32) -> u64 {
    5 + u64::from((i.wrapping_mul(37).wrapping_add(11)) % 56)
}

fn journal_path(db: &Path) -> PathBuf {
    let mut s = db.as_os_str().to_owned();
    s.push("-journal");
    PathBuf::from(s)
}

#[test]
fn kill_9_mid_write_loop_always_recovers_to_a_consistent_state() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("kill_9_mid_write_loop_always_recovers_to_a_consistent_state");
        return;
    };

    let db = scratch_db("torture");
    let status = Command::new(&oracle)
        .arg(&db)
        .arg("CREATE TABLE t(a INTEGER)")
        .status()
        .unwrap();
    assert!(status.success());

    let vfs = UnixVfs;
    // Page size is declared at header bytes 16-17 (big-endian), with
    // `1` special-cased to mean 65536 — same decoding `header.rs` does,
    // re-derived here rather than pulled in since this file only needs
    // the one number to open a `Pager` with.
    let page_size = {
        let pager = Pager::open(&vfs, &db, 4096).unwrap();
        let bytes = pager.read_page(1).unwrap();
        let declared = u16::from_be_bytes([bytes[16], bytes[17]]);
        if declared == 1 {
            65536
        } else {
            u32::from(declared)
        }
    };

    let mut previous_row_count: i64 = 0;

    for i in 0..iterations() {
        let mut child = Command::new(PROBE)
            .arg(&db)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawning {PROBE} {}: {e}", db.display()));

        std::thread::sleep(Duration::from_millis(delay_ms(i)));

        child
            .kill()
            .unwrap_or_else(|e| panic!("kill -9 on iteration {i}: {e}"));
        child.wait().ok();

        // Recovery happens transparently on open — a panic or Err here
        // is itself a torture-test failure, not just a skipped
        // assertion.
        {
            let recovered = Pager::open(&vfs, &db, page_size)
                .unwrap_or_else(|e| panic!("iteration {i}: Pager::open failed to recover: {e}"));
            drop(recovered);
        }

        // A *hot* journal (valid magic, real records) must be gone —
        // `recover_hot_journal` deletes it as its last step. A
        // zero-length journal is a different, harmless case (the
        // writer's `create_or_open_write` landed but the kill beat
        // even the header write): nothing was ever written that
        // needs rolling back, so `Pager::open` correctly leaves it
        // alone rather than guessing at "stale vs. hot" — verified
        // against real `sqlite3`, which does exactly the same (a
        // bare read-only open never touches it; the next writer's own
        // journal creation reclaims it). Only a *non-empty* leftover
        // would mean recovery silently failed to clean up after
        // itself.
        let jp = journal_path(&db);
        if vfs.exists(&jp).unwrap() {
            let size = vfs.open_read(&jp).unwrap().size().unwrap();
            assert_eq!(
                size, 0,
                "iteration {i}: non-empty -journal file still present after recovery"
            );
        }

        assert_integrity_check_ok(&oracle, &db);

        let row_count: i64 = {
            let output = Command::new(&oracle)
                .arg("-readonly")
                .arg(&db)
                .arg("SELECT count(*) FROM t")
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse()
                .unwrap()
        };
        assert!(
            row_count >= previous_row_count,
            "iteration {i}: row count went backwards ({previous_row_count} -> {row_count}) — a rollback undid a prior COMMIT's durable write"
        );
        previous_row_count = row_count;
    }

    std::fs::remove_dir_all(db.parent().unwrap()).ok();
}
