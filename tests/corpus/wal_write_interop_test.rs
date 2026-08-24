//! #386/#387 acceptance tests for the V6.2 WAL write path: crash-recovery
//! behavior of our own [`WalWriter`]-produced frames, and the "vice versa"
//! oracle-parity direction (`journal_interop_test.rs`'s pattern applied to
//! WAL) — a `-wal` file *we* write must be recoverable by a real
//! `sqlite3`, not just the other way around (`src/pager.rs`'s
//! `wal_pending*` fixture tests already prove sqlite3-written WALs read
//! correctly through our `Pager::open`).
//!
//! "Same data via WAL mode vs journal mode" and "checkpoint FULL/RESTART"
//! parity are out of scope here — `journal_mode=WAL` switching (#388) and
//! multi-reader/writer concurrency (#389) aren't implemented yet, so
//! there's no sqlite-rs-side WAL *mode* to compare against a journal-mode
//! run of the same workload. Tracked for #388/#389 instead of stubbed
//! speculatively here.
//!
//! `scratch_db`/`declared_page_size` are `pub(crate)` so
//! `wal_concurrent_interop_test.rs` (#390 — SQL-level live interop with a
//! real `sqlite3` process, one level up from this file's byte-level WAL
//! frame tests) can reuse them instead of duplicating.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::header::JournalMode;
use sqlite_rs::pager::checkpoint::checkpoint_passive;
use sqlite_rs::pager::wal::{self, WalHeader, WalWriter};
use sqlite_rs::pager::{Pager, PagerError};
use sqlite_rs::vfs::{companion_path, AnyVfs, PageSource, UnixVfs, VfsError, WritablePageSource};

use crate::oracle::{pinned_oracle, skip_no_oracle};

/// A lock held by a genuine second OS process — same technique as
/// `src/vfs/test_lock_probe.rs` (not reusable here: it's `pub(crate)`
/// inside the lib crate, invisible to this separate integration-test
/// binary), needed because POSIX record locks never conflict with a
/// second request from the *same* process.
struct HeldLock {
    child: Child,
    stdin: ChildStdin,
}

impl HeldLock {
    fn spawn(path: &std::path::Path, kind: &str, start: i64, len: i64) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lock_probe"))
            .args([
                "holdlock",
                &path.display().to_string(),
                kind,
                &start.to_string(),
                &len.to_string(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "locked", "lock_probe failed to claim the lock");
        HeldLock { child, stdin }
    }

    fn release(mut self) {
        self.stdin.write_all(b"\n").ok();
        self.child.wait().unwrap();
    }
}

pub(crate) fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-wal-write-interop-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

pub(crate) fn declared_page_size(vfs: &UnixVfs, db: &std::path::Path) -> u32 {
    let source = WritablePageSource::open(vfs, db, 4096).unwrap();
    let header = source.read_page(1).unwrap();
    let declared = u16::from_be_bytes([header[16], header[17]]) as u32;
    if declared == 1 {
        65536
    } else {
        declared
    }
}

/// A partial/torn frame (fewer bytes than a full frame, or a full frame
/// whose checksum is wrong because the write was cut short) must be
/// ignored — [`wal::committed_pages`] should return exactly the last
/// fully-verified commit, the same guarantee `crash_torture_test.rs`
/// exercises for the main file/rollback-journal path.
#[test]
fn partial_frame_after_a_committed_one_is_ignored() {
    let vfs = AnyVfs::new(sqlite_rs::vfs::MemoryVfs::new());
    let path = std::path::Path::new("/crash.db-wal");
    let page_size = 512u32;

    let header = WalHeader::new(true, page_size, 0xAAAA, 0xBBBB, 1);
    let mut writer = WalWriter::create(&vfs, path, header).unwrap();
    let committed_page = vec![0x11u8; page_size as usize];
    writer.append_frame(1, &committed_page, 1).unwrap();
    writer.sync().unwrap();

    // Simulate `kill -9` mid-append: a second frame's header made it to
    // disk but its page payload didn't.
    let file = vfs.open_write(path).unwrap();
    let full_len = file.size().unwrap();
    let torn_frame_header_only = full_len + 24; // header, no page payload
    file.write_at(&[0u8; 24], full_len).unwrap();
    let mut bytes = vec![0u8; torn_frame_header_only as usize];
    let n = file.read_at(&mut bytes, 0).unwrap();
    bytes.truncate(n);

    let parsed = WalHeader::parse(&bytes).unwrap();
    let (pages, db_size) = wal::committed_pages(&parsed, &bytes);
    assert_eq!(
        db_size, 1,
        "the torn tail must not override the last commit"
    );
    assert_eq!(pages.get(&1), Some(&committed_page));
}

/// A checkpoint that runs while an active reader still pins an older
/// frame must leave the database in a consistent state: exactly the
/// frames through that reader's mark are backfilled, nothing beyond it —
/// and a second checkpoint after the reader releases finishes the job.
#[test]
fn checkpoint_mid_write_is_consistent_then_completes_once_unblocked() {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-wal-checkpoint-midwrite-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("test.db");
    let page_size = 512u32;
    std::fs::write(&db_path, vec![0u8; page_size as usize]).unwrap();
    let shm_path = companion_path(&db_path, "-shm");
    std::fs::write(&shm_path, vec![0u8; 32768]).unwrap();

    let vfs = AnyVfs::new(UnixVfs);
    let wal_path = companion_path(&db_path, "-wal");
    let header = WalHeader::new(true, page_size, 0x5555, 0x6666, 1);
    let mut writer = WalWriter::create(&vfs, &wal_path, header).unwrap();
    writer
        .append_frame(1, &vec![0xAAu8; page_size as usize], 1)
        .unwrap();
    writer
        .append_frame(1, &vec![0xBBu8; page_size as usize], 1)
        .unwrap();
    writer.sync().unwrap();

    // A reader is pinned to frame 1 (slot 1's read-lock byte; see
    // `src/pager/checkpoint.rs`'s own test for the same byte-offset math).
    const WAL_READ_LOCK_SLOT_1_BYTE: i64 = 124;
    let held = HeldLock::spawn(&shm_path, "rdlock", WAL_READ_LOCK_SLOT_1_BYTE, 1);
    {
        use std::os::unix::fs::FileExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&shm_path)
            .unwrap();
        file.write_all_at(&1u32.to_ne_bytes(), 104).unwrap(); // slot 1's aReadMark
    }

    let mid = checkpoint_passive(&vfs, &db_path, page_size).unwrap();
    assert_eq!(mid.backfilled_frames, 1);
    assert!(!mid.checkpoint_complete);
    let db_file = vfs.open_read(&db_path).unwrap();
    let mut db_bytes = vec![0u8; page_size as usize];
    db_file.read_at(&mut db_bytes, 0).unwrap();
    assert_eq!(
        db_bytes,
        vec![0xAAu8; page_size as usize],
        "must not backfill past the pinned reader's frame"
    );

    held.release();

    let done = checkpoint_passive(&vfs, &db_path, page_size).unwrap();
    assert_eq!(done.backfilled_frames, 2);
    assert!(done.checkpoint_complete);
    let mut db_bytes = vec![0u8; page_size as usize];
    db_file.read_at(&mut db_bytes, 0).unwrap();
    assert_eq!(db_bytes, vec![0xBBu8; page_size as usize]);
}

/// #389: two concurrent *writers* must actually serialize through
/// `WAL_WRITE_LOCK`, not just readers-vs-writer (the WAL reader-mark tests
/// above). POSIX record locks never conflict with a second request from
/// the *same* process, so proving this requires a real second OS process
/// (`HeldLock`, same technique `checkpoint_mid_write_is_consistent_then_completes_once_unblocked`
/// uses for the reader-mark byte) holding `WAL_WRITE_LOCK` — offset
/// `UNIX_SHM_BASE` (120), 1 byte, matching `src/vfs/shm.rs`'s private
/// `WAL_WRITE_LOCK_BYTE` — while this crate's own [`Pager::flush`]
/// attempts to commit a WAL-mode transaction. The attempt must be
/// refused ([`VfsError::Locked`]), not silently interleave frames or
/// corrupt the WAL; once the lock is released, the same writer commits
/// successfully.
#[test]
fn concurrent_writer_is_refused_the_wal_write_lock() {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-wal-write-lock-contend-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("test.db");
    let page_size = 512u32;
    std::fs::write(&db_path, vec![0u8; page_size as usize]).unwrap();

    let vfs = UnixVfs;
    let mut pager = Pager::open(&vfs, &db_path, page_size).unwrap();
    pager.set_journal_mode(JournalMode::Wal).unwrap();

    let shm_path = companion_path(&db_path, "-shm");
    const WAL_WRITE_LOCK_BYTE: i64 = 120;
    let held = HeldLock::spawn(&shm_path, "wrlock", WAL_WRITE_LOCK_BYTE, 1);

    pager.get_page_mut(1).unwrap().fill(0xAB);
    let result = pager.flush();
    assert!(
        matches!(&result, Err(PagerError::Vfs(VfsError::Locked { .. }))),
        "expected VfsError::Locked while a second process holds WAL_WRITE_LOCK, got {result:?}"
    );

    held.release();

    // Once the contending process releases the lock, the same writer
    // (its dirty page untouched by the failed attempt) commits cleanly.
    pager.flush().unwrap();
    assert_eq!(
        pager.read_page(1).unwrap(),
        vec![0xABu8; page_size as usize].into()
    );
}

/// A `-wal` frame written by our own [`WalWriter`] must be readable by a
/// real `sqlite3` — the vice-versa half of the read-path's oracle parity
/// (`src/pager.rs`'s `wal_pending*` fixture tests cover sqlite3-written
/// WALs read by us). Builds a real WAL-mode database via the oracle,
/// snapshots a data page before an `UPDATE`, checkpoints the `UPDATE`
/// away with TRUNCATE (emptying the `-wal` but leaving `journal_mode=WAL`
/// active), then appends one frame of our own — restoring the
/// pre-`UPDATE` page — via [`WalWriter`]. A fresh `sqlite3` process must
/// see the pre-`UPDATE` row: proof our frame header/checksum/salts are
/// byte-compatible with stock SQLite's WAL reader, including the
/// wal-index recovery path it takes when `-shm`'s cached header no longer
/// matches the `-wal` file's own salts (exactly what happens here, since
/// [`WalHeader::new`] always mints fresh salts).
#[test]
fn our_wal_frame_recovers_through_stock_sqlite3() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("our_wal_frame_recovers_through_stock_sqlite3");
        return;
    };

    let db = scratch_db("recover");
    let status = Command::new(&oracle)
        .arg(&db)
        .arg("create table t(a integer, b text); insert into t values (1, 'before!');")
        .status()
        .unwrap();
    assert!(status.success());

    let vfs = UnixVfs;
    let page_size = declared_page_size(&vfs, &db);
    let source = WritablePageSource::open(&vfs, &db, page_size).unwrap();
    let page_count = {
        let header = source.read_page(1).unwrap();
        u32::from_be_bytes([header[28], header[29], header[30], header[31]])
    };
    let snapshot: Vec<Vec<u8>> = (1..=page_count)
        .map(|n| source.read_page(n).unwrap().to_vec())
        .collect();

    let status = Command::new(&oracle)
        .arg(&db)
        .arg(
            "PRAGMA journal_mode=WAL; UPDATE t SET b='after!' WHERE a=1; \
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .status()
        .unwrap();
    assert!(status.success());

    let source = WritablePageSource::open(&vfs, &db, page_size).unwrap();
    let updated_page_count = {
        let header = source.read_page(1).unwrap();
        u32::from_be_bytes([header[28], header[29], header[30], header[31]])
    };
    assert_eq!(
        updated_page_count, page_count,
        "an equal-length UPDATE must not change the page count"
    );

    // Page 1 always differs too (the journal-mode switch flips its format
    // bytes, and every commit bumps its change counter) — the page that
    // actually matters is the one holding the row's new text, found by
    // its content rather than by "first page that differs".
    let changed_page = (1..=page_count)
        .find(|&n| {
            let page = source.read_page(n).unwrap();
            page.windows(6).any(|w| w == b"after!") && *page != snapshot[(n - 1) as usize]
        })
        .expect("the UPDATE's new text must land on some page");
    let original_content = &snapshot[(changed_page - 1) as usize];

    let wal_path = companion_path(&db, "-wal");
    let any_vfs = AnyVfs::new(UnixVfs);
    let header = WalHeader::new(true, page_size, 0xC0FF_EE01, 0xC0FF_EE02, 2);
    let mut writer = WalWriter::create(&any_vfs, &wal_path, header).unwrap();
    writer
        .append_frame(changed_page, original_content, page_count)
        .unwrap();
    writer.sync().unwrap();

    let output = Command::new(&oracle)
        .arg("-readonly")
        .arg(&db)
        .arg("select b from t where a = 1;")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "before!",
        "stock sqlite3 must prefer our WAL frame's content over the checkpointed main file"
    );
}
