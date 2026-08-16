#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Acceptance oracle for #90 (V2 phase 3B — cursor + sorter + ephemeral
//! opcodes): hand-assembled `Program`s exercising full-scan, ORDER BY,
//! and DISTINCT against the corpus's real b-tree fixtures, matching
//! `TableCursor`'s own oracle-parity full scan
//! (`src/btree.rs::table_multipage_full_scan_matches_oracle`) row-for-row
//! through the VDBE cursor/sorter/ephemeral opcodes rather than a direct
//! `TableCursor` call. Hand-assembly (not codegen, #91) is this ticket's
//! own acceptance bar — see spec 009's `Traceability Note`.

use std::path::Path;
use std::rc::Rc;

use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::record::Value;
use sqlite_rs::vdbe::{
    execute, execute_with_db, Collation, Instruction, Opcode, Program, SortKeyColumn, P4,
};
use sqlite_rs::vfs::{PageSource, UnixVfs, Vfs, VfsPageSource};

fn open_db(fixture: &str) -> (Rc<dyn PageSource>, DatabaseHeader) {
    let path = Path::new("tests/corpus/fixtures/btrees").join(fixture);
    let vfs = UnixVfs;
    let file = vfs.open_read(&path).unwrap();
    let mut header_buf = [0u8; 100];
    file.read_at(&mut header_buf, 0).unwrap();
    let header = DatabaseHeader::parse(&header_buf).unwrap();
    let source = VfsPageSource::open(&vfs, &path, header.page_size).unwrap();
    (Rc::new(source), header)
}

/// `SELECT rowid, note FROM products` shape: `Init -> OpenRead -> Rewind
/// -> [Rowid, Column, ResultRow, Next] -> Halt`, mirroring the harvested
/// full-scan shape from `tools/opcodes-v2.json`.
#[test]
fn full_scan_program_matches_oracle_row_for_row() {
    let (source, header) = open_db("table_multipage.db");
    let program = Program::new(vec![
        /* 0 */ Instruction::new(Opcode::Init, 0, 1, 0),
        /* 1 */ Instruction::new(Opcode::OpenRead, 0, 2, 0), // cursor 0, root page 2
        /* 2 */ Instruction::new(Opcode::Rewind, 0, 7, 0), // jump to Halt(7) if empty
        /* 3 */ Instruction::new(Opcode::Rowid, 0, 0, 0), // r0 = rowid
        /* 4 */ Instruction::new(Opcode::Column, 0, 1, 1), // r1 = column 1 (note)
        /* 5 */ Instruction::new(Opcode::ResultRow, 0, 2, 0),
        /* 6 */ Instruction::new(Opcode::Next, 0, 3, 0), // loop to pc 3 if more rows
        /* 7 */ Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let rows = execute_with_db(&program, source, header).unwrap();

    assert_eq!(rows.len(), 3000);
    assert_eq!(
        rows[0],
        vec![Value::Integer(1), Value::Text("row number 1".to_string())]
    );
    assert_eq!(
        rows[2999],
        vec![
            Value::Integer(3000),
            Value::Text("row number 3000".to_string())
        ]
    );
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row[0], Value::Integer((i + 1) as i64));
    }
}

/// `SELECT rowid FROM products ORDER BY rowid DESC` shape: buffers
/// every row into the sorter, sorts once, then emits from the sorted
/// order via `OpenPseudo` + `SorterData`/`Column`.
#[test]
fn order_by_program_emits_rows_in_sorted_order() {
    let (source, header) = open_db("table_multipage.db");
    let program = Program::new(vec![
        /* 0 */ Instruction::new(Opcode::Init, 0, 1, 0),
        /* 1 */ Instruction::new(Opcode::OpenRead, 0, 2, 0), // cursor 0: table
        /* 2 */
        Instruction::with_p4(
            Opcode::SorterOpen,
            1,
            0,
            0,
            P4::SortKey(vec![SortKeyColumn {
                index: 0,
                descending: true,
                collation: Collation::Binary,
                nulls_first: false,
            }]),
        ), // cursor 1: sorter, key = column 0 descending
        /* 3 */
        Instruction::new(Opcode::OpenPseudo, 2, 10, 0), // cursor 2: pseudo, reads register 10
        // Scan pass: buffer every row's rowid into the sorter.
        /* 4 */
        Instruction::new(Opcode::Rewind, 0, 9, 0), // jump to SorterSort(9) if empty
        /* 5 */ Instruction::new(Opcode::Rowid, 0, 0, 0),
        /* 6 */ Instruction::new(Opcode::MakeRecord, 0, 1, 1),
        /* 7 */ Instruction::new(Opcode::SorterInsert, 1, 1, 0),
        /* 8 */ Instruction::new(Opcode::Next, 0, 5, 0),
        // Output pass: sorted rowids, descending.
        /* 9 */
        Instruction::new(Opcode::SorterSort, 1, 14, 0), // jump to Halt(14) if empty
        /* 10 */
        Instruction::new(Opcode::SorterData, 1, 10, 0), // r10 = current sorted record
        /* 11 */
        Instruction::new(Opcode::Column, 2, 0, 11), // r11 = pseudo cursor's column 0
        /* 12 */ Instruction::new(Opcode::ResultRow, 11, 1, 0),
        /* 13 */
        Instruction::new(Opcode::SorterNext, 1, 10, 0), // jump to pc 10 if more rows
        /* 14 */ Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let rows = execute_with_db(&program, source, header).unwrap();

    assert_eq!(rows.len(), 3000);
    assert_eq!(rows[0], vec![Value::Integer(3000)]);
    assert_eq!(rows[1], vec![Value::Integer(2999)]);
    assert_eq!(rows[2999], vec![Value::Integer(1)]);
}

/// `SELECT DISTINCT note FROM products` shape (against an in-memory-only
/// program, no real table needed): probes an ephemeral index before
/// each emit, discarding rows already seen. Each 4-instruction candidate
/// block (`String8`, `Found`, `ResultRow`, `IdxInsert`) is the same
/// shape; `Found`'s jump target is simply the next block's start, so a
/// duplicate skips straight past its own `ResultRow`/`IdxInsert`.
#[test]
fn distinct_program_discards_rows_already_seen() {
    let program = Program::new(vec![
        /* 0 */ Instruction::new(Opcode::Init, 0, 1, 0),
        /* 1 */ Instruction::new(Opcode::OpenEphemeral, 0, 0, 0), // cursor 0
        // Candidate "a" (first occurrence: absent, insert, emit).
        /* 2 */
        Instruction::with_p4(Opcode::String8, 0, 0, 0, P4::Str("a".to_string())),
        /* 3 */
        Instruction::with_p4(Opcode::Found, 0, 6, 0, P4::Int(1)), // jump to pc 6 if present
        /* 4 */ Instruction::new(Opcode::ResultRow, 0, 1, 0),
        /* 5 */
        Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)),
        // Candidate "a" (second occurrence: present, skip emit/insert).
        /* 6 */
        Instruction::with_p4(Opcode::String8, 0, 0, 0, P4::Str("a".to_string())),
        /* 7 */
        Instruction::with_p4(Opcode::Found, 0, 10, 0, P4::Int(1)), // jump to pc 10 if present
        /* 8 */
        Instruction::new(Opcode::ResultRow, 0, 1, 0), // unreachable (found=true)
        /* 9 */
        Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)), // unreachable
        // Candidate "b" (absent, insert, emit).
        /* 10 */
        Instruction::with_p4(Opcode::String8, 0, 0, 0, P4::Str("b".to_string())),
        /* 11 */
        Instruction::with_p4(Opcode::Found, 0, 14, 0, P4::Int(1)), // jump to pc 14 (Halt) if present
        /* 12 */ Instruction::new(Opcode::ResultRow, 0, 1, 0),
        /* 13 */
        Instruction::with_p4(Opcode::IdxInsert, 0, 0, 0, P4::Int(1)),
        /* 14 */ Instruction::new(Opcode::Halt, 0, 0, 0),
    ]);
    let rows = execute(&program).unwrap();

    assert_eq!(
        rows,
        vec![
            vec![Value::Text("a".to_string())],
            vec![Value::Text("b".to_string())],
        ]
    );
}
