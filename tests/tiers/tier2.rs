// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! Tier 2 — WRITE CORE (spec 001-architecture Tier Model, `plan.md` Core
//! Definition): CRUD on rowid tables, basic constraints, rollback-journal
//! transactions, `integrity_check`-clean output. Simplifiable, not
//! droppable — every clause below is a stub today, filling in through
//! V3/V5 per `plan.md`'s Value Blocks table.

#[path = "../corpus/oracle.rs"]
#[allow(dead_code)]
mod oracle;

use oracle::{assert_integrity_check_ok, pinned_oracle, skip_no_oracle};
use sqlite_rs::pager::journal::{page_checksum, JournalHeader, JOURNAL_HEADER_LEN};
use sqlite_rs::pager::Pager;
use sqlite_rs::vfs::{MemoryVfs, PageSource, Vfs};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
const CLI: &str = env!("CARGO_BIN_EXE_sqlite-rs");

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-tier2-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("scratch.db")
}

fn run_exec(db: &Path, sql: &str) -> Output {
    Command::new(CLI)
        .arg("exec")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} exec {} {sql:?}: {e}", db.display()))
}

fn run_query(db: &Path, sql: &str) -> String {
    let output = Command::new(CLI)
        .arg("query")
        .arg(db)
        .arg(sql)
        .output()
        .unwrap_or_else(|e| panic!("running {CLI} query {} {sql:?}: {e}", db.display()));
    assert!(
        output.status.success(),
        "query {sql:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A scratch db seeded via the pinned oracle if available, else via our
/// own CLI's CREATE TABLE — either way, gives every test a real on-disk
/// database with a valid header before it starts exercising `exec`.
fn seed_db(label: &str, ddl: &str) -> PathBuf {
    let db = scratch_db(label);
    if let Some(oracle) = pinned_oracle() {
        let status = Command::new(&oracle).arg(&db).arg(ddl).status().unwrap();
        assert!(status.success());
    } else {
        assert!(run_exec(&db, "CREATE TABLE seed_bootstrap(x)")
            .status
            .success());
        assert!(run_exec(&db, ddl).status.success());
    }
    db
}

/// #217 — the exit-gate proof that phases 1-3's CRUD path holds together
/// end-to-end on a rowid table, via the CLI surface #215 wired up.
#[test]
fn t2_crud_round_trips_on_rowid_tables() {
    let db = seed_db("crud", "CREATE TABLE t(a INTEGER, b TEXT)");
    assert!(
        run_exec(&db, "INSERT INTO t VALUES (1, 'x'), (2, 'y'), (3, 'z')")
            .status
            .success()
    );
    assert!(run_exec(&db, "UPDATE t SET b = 'yy' WHERE a = 2")
        .status
        .success());
    assert!(run_exec(&db, "DELETE FROM t WHERE a = 3").status.success());

    let rows = run_query(&db, "SELECT * FROM t");
    assert_eq!(rows, "1|x\n2|yy\n");
}

/// #217 — every file sqlite-rs writes must be `PRAGMA integrity_check`-ed
/// clean by stock `sqlite3` (epic #161's acceptance gate), proven here via
/// the shared oracle helper centralized in #216.
#[test]
fn t2_written_file_passes_integrity_check() {
    let Some(oracle) = pinned_oracle() else {
        skip_no_oracle("t2_written_file_passes_integrity_check");
        return;
    };
    let db = seed_db("integrity", "CREATE TABLE t(a INTEGER, b TEXT)");
    assert!(run_exec(&db, "INSERT INTO t VALUES (1, 'x'), (2, 'y')")
        .status
        .success());
    assert!(run_exec(&db, "CREATE INDEX idx_t_a ON t(a)")
        .status
        .success());
    assert!(run_exec(&db, "UPDATE t SET b = 'yy' WHERE a = 2")
        .status
        .success());
    assert!(run_exec(&db, "DELETE FROM t WHERE a = 1").status.success());

    assert_integrity_check_ok(&oracle, &db);
}

/// #172 — the weak half of statement atomicity: a statement whose writes
/// never reach [`Pager::flush`] (dirty pages live only in the in-memory
/// `dirty` map) leaves the on-disk database byte-identical, because
/// nothing was ever written to it. The strong half — a statement that DID
/// reach `flush`, was interrupted partway through overwriting the main
/// file, and is rolled back on next open via its rollback journal — is
/// `t2_journal_transactions_commit_and_rollback`'s second half below.
#[test]
fn t2_statement_atomicity() {
    let mut vfs = MemoryVfs::new();
    let page_size = 512u32;
    let original = vec![1u8; page_size as usize];
    vfs.insert("/test.db", original.clone());

    let mut pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();
    let page = pager.get_page_mut(1).unwrap();
    page.fill(0xFF);
    drop(pager); // the statement "fails" before ever calling flush()

    let reopened = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();
    assert_eq!(reopened.read_page(1).unwrap(), original.into());
}

/// #172 — [`Pager::flush`] is the commit boundary: a transaction that
/// reaches it is durable and leaves no journal behind (commit half); a
/// transaction crashing between "journal synced" and "main file synced"
/// is rolled back to its pre-transaction state the next time the
/// database is opened (rollback half), via the same hot-journal recovery
/// path proven against a real `sqlite3`-written journal in
/// `tests/tiers/tier0.rs::t0_hot_journal_recovers_committed_state`.
#[test]
fn t2_journal_transactions_commit_and_rollback() {
    let mut vfs = MemoryVfs::new();
    let page_size = 512u32;
    let mut db = vec![0u8; page_size as usize];
    db[28..32].copy_from_slice(&2u32.to_be_bytes()); // page count = 2
    db.extend(vec![1u8; page_size as usize]); // page 2's original content
    vfs.insert("/test.db", db);

    // Commit half: a transaction that reaches flush() is durable, and
    // the journal it wrote along the way is gone afterward.
    {
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();
        let page = pager.get_page_mut(2).unwrap();
        page.fill(2u8);
        pager.flush().unwrap();
    }
    assert!(!vfs.exists(Path::new("/test.db-journal")).unwrap());
    {
        let pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();
        assert_eq!(
            pager.read_page(2).unwrap(),
            vec![2u8; page_size as usize].into()
        );
    }

    // Rollback half: simulate a crash between "journal synced" and "main
    // file synced" — the main file already shows a torn write to page 2,
    // but a well-formed journal recording page 2's pre-transaction
    // content (the "2u8" bytes just committed above) is still present.
    let torn_page = vec![0xEEu8; page_size as usize];
    let db_file = vfs.open_write(Path::new("/test.db")).unwrap();
    db_file.write_at(&torn_page, page_size as u64).unwrap();

    let pre_image = vec![2u8; page_size as usize];
    let nonce = 99;
    let header = JournalHeader {
        n_rec: 1,
        nonce,
        initial_page_count: 2,
        sector_size: page_size,
        page_size,
    }
    .serialize(JOURNAL_MAGIC);
    let mut journal_bytes = vec![0u8; page_size as usize];
    journal_bytes[..JOURNAL_HEADER_LEN].copy_from_slice(&header);
    journal_bytes.extend_from_slice(&2u32.to_be_bytes());
    journal_bytes.extend_from_slice(&pre_image);
    journal_bytes.extend_from_slice(&page_checksum(nonce, &pre_image).to_be_bytes());
    vfs.insert("/test.db-journal", journal_bytes);

    let recovered = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();
    assert_eq!(recovered.read_page(2).unwrap(), pre_image.into());
    assert!(!vfs.exists(Path::new("/test.db-journal")).unwrap());
}
