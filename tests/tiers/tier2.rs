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

use sqlite_rs::pager::journal::{page_checksum, JournalHeader, JOURNAL_HEADER_LEN};
use sqlite_rs::pager::Pager;
use sqlite_rs::vfs::{MemoryVfs, PageSource, Vfs};
use std::path::Path;

const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

#[test]
#[ignore = "V3 — CREATE/INSERT/UPDATE/DELETE round-trip"]
fn t2_crud_round_trips_on_rowid_tables() {
    unimplemented!()
}

#[test]
#[ignore = "V3 — written file passes stock sqlite3 PRAGMA integrity_check"]
fn t2_written_file_passes_integrity_check() {
    unimplemented!()
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
    assert_eq!(reopened.read_page(1).unwrap(), original);
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
        assert_eq!(pager.read_page(2).unwrap(), vec![2u8; page_size as usize]);
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
    assert_eq!(recovered.read_page(2).unwrap(), pre_image);
    assert!(!vfs.exists(Path::new("/test.db-journal")).unwrap());
}
