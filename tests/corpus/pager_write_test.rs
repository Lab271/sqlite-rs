//! #166 compatibility proof: a page flushed through `Pager::flush` must
//! still be a file stock `sqlite3` opens and `PRAGMA integrity_check`s
//! cleanly. `run_oracle` (`oracle.rs`) is deliberately read-only, so this
//! file shells out to the oracle binary directly for the one write
//! (`sqlite3 <db> "..."`, no `-readonly`) that seeds the fixture.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::pager::Pager;
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::oracle::{pinned_oracle, skip_no_oracle};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-pager-write-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

/// Round-trips a page through `Pager::get_page_mut` + `flush` unchanged
/// (this ticket adds no b-tree-aware write support yet — that starts at
/// #168) and confirms stock `sqlite3` still opens the file, passes
/// `integrity_check`, and reads back the same row it wrote.
#[test]
fn flushed_page_still_opens_and_integrity_checks_in_stock_sqlite3() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("flushed_page_still_opens_and_integrity_checks_in_stock_sqlite3");
        return;
    };

    let db = scratch_db("roundtrip");
    let status = Command::new(&oracle)
        .arg(&db)
        .arg("create table t(a integer, b text); insert into t values (1, 'one');")
        .status()
        .unwrap();
    assert!(status.success());

    let vfs = UnixVfs;
    let page_size = {
        let pager = Pager::open(&vfs, &db, 4096).unwrap();
        let header = pager.read_page(1).unwrap();
        u16::from_be_bytes([header[16], header[17]]) as u32
    };
    // 1 (special-cased 65536) mirrors `header.rs`'s page-size decoding.
    let page_size = if page_size == 1 { 65536 } else { page_size };

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        let page1 = pager.get_page_mut(1).unwrap().clone();
        *pager.get_page_mut(1).unwrap() = page1;
        pager.flush().unwrap();
    }

    let integrity = Command::new(&oracle)
        .arg("-readonly")
        .arg(&db)
        .arg("PRAGMA integrity_check;")
        .output()
        .unwrap();
    assert!(integrity.status.success());
    assert_eq!(String::from_utf8_lossy(&integrity.stdout).trim(), "ok");

    let select = Command::new(&oracle)
        .arg("-readonly")
        .arg("-list")
        .arg(&db)
        .arg("select a, b from t;")
        .output()
        .unwrap();
    assert!(select.status.success());
    assert_eq!(String::from_utf8_lossy(&select.stdout).trim(), "1|one");

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}

/// #167 acceptance criteria: allocating a page (extending the file) and
/// then deallocating it (returning it to the freelist) must still leave a
/// file stock `sqlite3` opens and `PRAGMA integrity_check`s cleanly, with
/// the freelist page count reflecting the round trip.
#[test]
fn allocate_then_deallocate_page_still_integrity_checks_in_stock_sqlite3() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("allocate_then_deallocate_page_still_integrity_checks_in_stock_sqlite3");
        return;
    };

    let db = scratch_db("freelist");
    let status = Command::new(&oracle)
        .arg(&db)
        .arg("create table t(a integer, b text); insert into t values (1, 'one');")
        .status()
        .unwrap();
    assert!(status.success());

    let vfs = UnixVfs;
    let page_size = {
        let pager = Pager::open(&vfs, &db, 4096).unwrap();
        let header = pager.read_page(1).unwrap();
        u16::from_be_bytes([header[16], header[17]]) as u32
    };
    let page_size = if page_size == 1 { 65536 } else { page_size };

    {
        let mut pager = Pager::open(&vfs, &db, page_size).unwrap();
        let allocated = pager.allocate_page().unwrap();
        pager.flush().unwrap();
        pager.deallocate_page(allocated).unwrap();
        pager.flush().unwrap();
    }

    let integrity = Command::new(&oracle)
        .arg("-readonly")
        .arg(&db)
        .arg("PRAGMA integrity_check;")
        .output()
        .unwrap();
    assert!(integrity.status.success());
    assert_eq!(String::from_utf8_lossy(&integrity.stdout).trim(), "ok");

    let select = Command::new(&oracle)
        .arg("-readonly")
        .arg("-list")
        .arg(&db)
        .arg("select a, b from t;")
        .output()
        .unwrap();
    assert!(select.status.success());
    assert_eq!(String::from_utf8_lossy(&select.stdout).trim(), "1|one");

    let freelist_count = Command::new(&oracle)
        .arg("-readonly")
        .arg("-list")
        .arg(&db)
        .arg("PRAGMA freelist_count;")
        .output()
        .unwrap();
    assert!(freelist_count.status.success());
    assert_eq!(String::from_utf8_lossy(&freelist_count.stdout).trim(), "1");

    std::fs::remove_dir_all(db.parent().unwrap()).unwrap();
}
