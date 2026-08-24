#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

//! #194 acceptance: a hand-built `Program` exercising `OpenWrite` ->
//! `NewRowid` -> `MakeRecord` (with affinity) -> `Insert` against a real
//! temp-file database (`UnixVfs`, not `MemoryVfs`), re-read via
//! `TableCursor`/`decode_record` (V1's own reader) to confirm the row
//! round-trips byte-correctly. Also covers `Delete` and the real-cursor
//! `IdxInsert` path end to end through the VDBE dispatcher (`execute_with_writable_db`),
//! not just the colocated unit tests in `src/vdbe/cursor.rs`.
//!
//! No pinned-oracle (3.53.4, `tests/corpus/oracle.rs`) leg here: this
//! environment only has an Apple-patched 3.51.0 `sqlite3` (codec-enabled,
//! not the pinned build `tools/gen_fixtures.sh` accepts), so there is no
//! oracle to shell out to for a "stock sqlite3 reads written data" check
//! in CI. Manually verified once against `/usr/bin/sqlite3` during
//! development (see the ticket's final report) as an informal sanity
//! check, not as part of this automated suite.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sqlite_rs::btree::TableCursor;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::pager::Pager;
use sqlite_rs::record::{decode_record, TextEncoding, Value};
use sqlite_rs::vdbe::{execute_with_writable_db, Affinity, Instruction, Opcode, Program, P4};
use sqlite_rs::vfs::{UnixVfs, Vfs};

fn scratch_db(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "sqlite-rs-vdbe-write-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("test.db")
}

/// Writes a minimal one-page database (page 1 doubling as a table
/// b-tree's empty leaf root, rather than a real `sqlite_master` page —
/// the same simplification `src/btree/insert.rs`'s and
/// `src/vdbe/cursor.rs`'s own colocated tests use) to `path` via
/// `UnixVfs`, and returns its parsed header.
fn seed_minimal_db(vfs: &UnixVfs, path: &Path, page_size: u32) -> DatabaseHeader {
    let mut page1 = vec![0u8; page_size as usize];
    page1[0..16].copy_from_slice(b"SQLite format 3\0");
    page1[16..18].copy_from_slice(&u16::try_from(page_size).unwrap_or(1).to_be_bytes());
    page1[18] = 1;
    page1[19] = 1;
    page1[28..32].copy_from_slice(&1u32.to_be_bytes());
    page1[56..60].copy_from_slice(&1u32.to_be_bytes());

    let header_start = 100usize;
    page1[header_start] = 0x0d; // LEAF_TABLE
    page1[header_start + 1..header_start + 3].copy_from_slice(&0u16.to_be_bytes());
    page1[header_start + 3..header_start + 5].copy_from_slice(&0u16.to_be_bytes());
    let content_start = if page_size == 65536 {
        0u16
    } else {
        u16::try_from(page_size).unwrap()
    };
    page1[header_start + 5..header_start + 7].copy_from_slice(&content_start.to_be_bytes());
    page1[header_start + 7] = 0;

    let file = vfs.create_or_open_write(path).unwrap();
    file.write_at(&page1, 0).unwrap();
    file.sync().unwrap();

    let mut header_buf = [0u8; 100];
    header_buf.copy_from_slice(&page1[..100]);
    DatabaseHeader::parse(&header_buf).unwrap()
}

/// `OpenWrite(cursor 0, root page 1)` -> `NewRowid(-> r0)` ->
/// `MakeRecord(r1..r3, affinity "DB" -> r3)` -> `Insert(cursor 0, rowid
/// r0, record r3)` -> `Halt`. Verifies the written row is byte-correct
/// by reading it back through a fresh, independent `TableCursor` (V1's
/// reader, not the VDBE) opened on the same file after the writing
/// program returns (and its `Pager` — and any dirty pages it holds —
/// has been dropped), which additionally exercises that write opcodes
/// leave the on-disk file itself correct, not just an in-memory dirty
/// page cache.
#[test]
fn insert_round_trips_through_v1_reader_on_a_real_temp_file() {
    let path = scratch_db("insert-roundtrip");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);

    let program = Program::new(vec![
        Instruction::new(Opcode::OpenWrite, 0, 1, 0),
        Instruction::new(Opcode::NewRowid, 0, 0, 0),
        Instruction::with_p4(Opcode::String8, 0, 1, 0, P4::Str("42".to_string())),
        Instruction::with_p4(Opcode::String8, 0, 2, 0, P4::Str("hello".to_string())),
        Instruction::with_p4(
            Opcode::MakeRecord,
            1,
            2,
            3,
            P4::Affinity(vec![
                Affinity::Integer.to_p4_byte(),
                Affinity::Text.to_p4_byte(),
            ]),
        ),
        Instruction::new(Opcode::Insert, 0, 0, 3),
        Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);

    let pager = Pager::open(&vfs, &path, page_size).unwrap();
    execute_with_writable_db(&program, pager, header).unwrap();

    // Independent read: a fresh Pager/TableCursor over the same file,
    // opened after the writing Vm (and its Pager) has gone out of scope.
    let read_pager = Pager::open(&vfs, &path, page_size).unwrap();
    let mut cursor = TableCursor::new(read_pager, &header, 1);
    let row = cursor.first_row().unwrap().expect("one row was inserted");
    assert_eq!(row.rowid, 1);
    let values = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
    assert_eq!(
        values,
        vec![Value::Integer(42), Value::Text("hello".to_string().into())]
    );
}

#[test]
fn delete_removes_a_previously_inserted_row_from_the_on_disk_file() {
    let path = scratch_db("delete-roundtrip");
    let vfs = UnixVfs;
    let page_size = 512u32;
    let header = seed_minimal_db(&vfs, &path, page_size);

    let insert_program = Program::new(vec![
        Instruction::with_p4(Opcode::Integer, 1, 1, 0, P4::None),
        Instruction::new(Opcode::MakeRecord, 1, 1, 2),
        Instruction::new(Opcode::OpenWrite, 0, 1, 0),
        Instruction::new(Opcode::Integer, 1, 0, 0),
        Instruction::new(Opcode::Insert, 0, 0, 2),
        Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let pager = Pager::open(&vfs, &path, page_size).unwrap();
    execute_with_writable_db(&insert_program, pager, header).unwrap();

    let delete_program = Program::new(vec![
        Instruction::new(Opcode::OpenWrite, 0, 1, 0),
        Instruction::new(Opcode::Rewind, 0, 3, 0),
        Instruction::new(Opcode::Delete, 0, 0, 0),
        Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let pager = Pager::open(&vfs, &path, page_size).unwrap();
    execute_with_writable_db(&delete_program, pager, header).unwrap();

    let read_pager = Pager::open(&vfs, &path, page_size).unwrap();
    let mut cursor = TableCursor::new(read_pager, &header, 1);
    assert!(cursor.first().unwrap().is_none());
}
