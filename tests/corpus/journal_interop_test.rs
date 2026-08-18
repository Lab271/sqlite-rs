//! #172 cross-compat proof, the "vice versa" half: a rollback journal
//! *we* write must be recoverable by a real `sqlite3`, not just the
//! other way around (`tests/tiers/tier0.rs::t0_hot_journal_recovers_committed_state`
//! and `src/pager.rs`'s fixture test prove sqlite3-written journals
//! recover through our `Pager::open`).
//!
//! Simulates a crash between "journal synced" and "main file synced":
//! writes a journal via [`JournalWriter`] recording a page's
//! pre-transaction content, then corrupts that page in the main file
//! (the torn write a real crash would leave behind), then lets a real
//! `sqlite3` open the database — its own hot-journal detection should
//! transparently roll back to the pre-transaction content and delete the
//! journal, with no special recovery command needed on the caller's
//! part.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::pager::journal::JournalWriter;
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, WritablePageSource};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-journal-interop-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

#[test]
fn our_journal_recovers_through_stock_sqlite3() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("our_journal_recovers_through_stock_sqlite3");
        return;
    };

    let db = scratch_db("recover");
    let status = Command::new(&oracle)
        .arg(&db)
        .arg("create table t(a integer, b text); insert into t values (1, 'committed-before');")
        .status()
        .unwrap();
    assert!(status.success());

    let vfs = UnixVfs;
    let page_size = {
        let source = WritablePageSource::open(&vfs, &db, 4096).unwrap();
        let header = source.read_page(1).unwrap();
        let declared = u16::from_be_bytes([header[16], header[17]]) as u32;
        if declared == 1 {
            65536
        } else {
            declared
        }
    };

    let source = WritablePageSource::open(&vfs, &db, page_size).unwrap();
    let page_count = {
        let header = source.read_page(1).unwrap();
        u32::from_be_bytes([header[28], header[29], header[30], header[31]])
    };
    let original_last_page = source.read_page(page_count).unwrap();

    let journal_path = db.with_file_name(format!(
        "{}-journal",
        db.file_name().unwrap().to_str().unwrap()
    ));
    let writer = JournalWriter::create(
        &vfs,
        &journal_path,
        page_size,
        page_size,
        page_count,
        1,
        0xC0FF_EE42,
    )
    .unwrap();
    writer
        .write_record(0, page_count, &original_last_page)
        .unwrap();
    writer.sync().unwrap();

    // The torn write a real crash mid-flush would leave behind.
    let torn = vec![0x55u8; page_size as usize];
    source.write_page(page_count, &torn).unwrap();
    source.sync().unwrap();

    // A real sqlite3 opening this database must transparently detect the
    // hot journal, roll it back, and see the original committed row.
    let output = Command::new(&oracle)
        .arg(&db)
        .arg("select a, b from t;")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "1|committed-before"
    );

    assert!(
        !vfs.exists(&journal_path).unwrap(),
        "sqlite3 must delete the journal after recovering it (DELETE mode)"
    );
}
