//! Property-based tests fuzzing raw b-tree page bytes through
//! `TableCursor` (issue #179).
//!
//! Lives outside `src/` for the same reason as `record_proptest.rs`: it's
//! outside the qualified subset (issue #23), whose curated macro allowlist
//! doesn't include proptest's `proptest!` macro expansion.
//!
//! `TableCursor` only needs a `PageSource` impl, not a full pager/VFS/file
//! — so pages are built directly as in-memory byte buffers and served via
//! a `FakePageSource`, mirroring the private test helper in `src/btree.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::collections::HashMap;
use std::rc::Rc;

use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use sqlite_rs::btree::TableCursor;
use sqlite_rs::header::DatabaseHeader;
use sqlite_rs::record::TextEncoding;
use sqlite_rs::vfs::{PageError, PageSource};

const PAGE_SIZE: usize = 512;
const LEAF_TABLE: u8 = 0x0d;
const INTERIOR_TABLE: u8 = 0x05;

struct FakePageSource {
    pages: HashMap<u32, Vec<u8>>,
}

impl PageSource for FakePageSource {
    fn read_page(&self, page_num: u32) -> Result<Rc<[u8]>, PageError> {
        self.pages
            .get(&page_num)
            .map(|page| Rc::from(page.as_slice()))
            .ok_or(PageError::InvalidPageNumber)
    }
}

fn fake_header() -> DatabaseHeader {
    DatabaseHeader {
        page_size: PAGE_SIZE as u32,
        write_version: 1,
        read_version: 1,
        reserved_space: 0,
        page_count: 4,
        freelist_trunk_page: 0,
        freelist_page_count: 0,
        schema_cookie: 0,
        schema_format: 0,
        largest_root_btree_page: 0,
        text_encoding: TextEncoding::Utf8,
        user_version: 0,
        application_id: 0,
    }
}

/// Minimal-length varint encoder, same bit layout as `record_proptest.rs`'s
/// `encode_varint` (7 bits/byte, high-bit continuation flag).
fn encode_varint(value: u64) -> Vec<u8> {
    if value < 0x80 {
        return vec![value as u8];
    }
    let mut groups = Vec::new();
    let mut v = value;
    loop {
        groups.push((v & 0x7f) as u8);
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    groups.reverse();
    let last = groups.len() - 1;
    groups
        .iter()
        .enumerate()
        .map(|(i, &g)| if i == last { g } else { g | 0x80 })
        .collect()
}

/// One raw table b-tree cell: `payload_len` and `rowid` varints followed by
/// `local_len` bytes of local payload, optionally capped off with a 4-byte
/// (possibly bogus) overflow-page pointer.
#[derive(Debug, Clone)]
struct RawCell {
    payload_len: u64,
    rowid: u64,
    local_len: u16,
    overflow_ptr: Option<u32>,
}

fn raw_cell_strategy() -> impl Strategy<Value = RawCell> {
    (
        any::<u64>(),
        any::<u64>(),
        any::<u16>(),
        proptest::option::of(any::<u32>()),
    )
        .prop_map(|(payload_len, rowid, local_len, overflow_ptr)| RawCell {
            payload_len,
            rowid,
            local_len,
            overflow_ptr,
        })
}

fn encode_cell(cell: &RawCell) -> Vec<u8> {
    let mut bytes = encode_varint(cell.payload_len);
    bytes.extend(encode_varint(cell.rowid));
    bytes.extend(std::iter::repeat_n(0u8, cell.local_len as usize));
    if let Some(ptr) = cell.overflow_ptr {
        bytes.extend_from_slice(&ptr.to_be_bytes());
    }
    bytes
}

/// Builds an arbitrary leaf-table page: a fuzzed page-type byte, a fuzzed
/// `num_cells`, and a fuzzed cell-pointer array (which may point anywhere
/// in the page, including out of bounds or overlapping), followed by
/// however many raw cells fit after the pointer array.
fn build_page(
    page_type: u8,
    num_cells_field: u16,
    cell_ptr_offsets: &[u16],
    cells: &[RawCell],
) -> Vec<u8> {
    let mut page = vec![0u8; PAGE_SIZE];
    page[0] = page_type;
    page[3..5].copy_from_slice(&num_cells_field.to_be_bytes());

    let header_len = if page_type == INTERIOR_TABLE { 12 } else { 8 };
    let cell_ptr_base = header_len;

    for (i, &off) in cell_ptr_offsets.iter().enumerate() {
        let ptr_off = cell_ptr_base + i * 2;
        if ptr_off + 2 <= PAGE_SIZE {
            page[ptr_off..ptr_off + 2].copy_from_slice(&off.to_be_bytes());
        }
    }

    // Lay actual cell bytes starting right after the pointer array; the
    // fuzzed offsets above may or may not agree with where cells actually
    // land, which is exactly the point (dangling/garbage pointers).
    let mut cursor = cell_ptr_base + cell_ptr_offsets.len() * 2;
    for cell in cells {
        let bytes = encode_cell(cell);
        let end = cursor.saturating_add(bytes.len());
        if end <= PAGE_SIZE {
            page[cursor..end].copy_from_slice(&bytes);
        }
        cursor = end;
    }

    page
}

/// Drives a cursor through every row via `first()`/`next()`, asserting the
/// walk terminates with either a full traversal (`Ok(None)`) or a typed
/// `BtreeError` — never a panic.
fn drive_cursor(source: FakePageSource, root_page: u32) {
    let header = fake_header();
    let mut cursor = TableCursor::new(source, &header, root_page);

    let mut step = cursor.first();
    loop {
        match step {
            Ok(Some(_row)) => {
                step = cursor.next();
            }
            Ok(None) => break,
            Err(_e) => break,
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "tests/proptest/proptest-regressions/btree_proptest.txt"
        ))),
        ..ProptestConfig::default()
    })]

    /// A single arbitrary leaf page (page type, num_cells, cell-pointer
    /// array, and cell bodies all fuzzed independently) never panics when
    /// driven through `TableCursor` — only ever yields rows or a typed
    /// `BtreeError`.
    #[test]
    fn arbitrary_leaf_page_never_panics(
        page_type in prop_oneof![Just(LEAF_TABLE), Just(INTERIOR_TABLE), any::<u8>()],
        num_cells_field in any::<u16>(),
        cell_ptr_offsets in prop::collection::vec(any::<u16>(), 0..8),
        cells in prop::collection::vec(raw_cell_strategy(), 0..4),
    ) {
        let page = build_page(page_type, num_cells_field, &cell_ptr_offsets, &cells);
        let mut pages = HashMap::new();
        pages.insert(2u32, page);
        let source = FakePageSource { pages };

        drive_cursor(source, 2);
    }

    /// A two-page setup (root leaf page + a second page reachable via an
    /// overflow pointer) whose overflow-chain fields are arbitrary,
    /// including self-referential and out-of-range pointers — exercising
    /// the overflow-chain cycle/bounds guards in `reassemble_payload`.
    #[test]
    fn arbitrary_overflow_chain_never_panics(
        num_cells_field in any::<u16>(),
        cell_ptr_offsets in prop::collection::vec(any::<u16>(), 0..4),
        cells in prop::collection::vec(raw_cell_strategy(), 0..3),
        overflow_next in any::<u32>(),
        overflow_body in prop::collection::vec(any::<u8>(), 0..PAGE_SIZE),
    ) {
        let leaf = build_page(LEAF_TABLE, num_cells_field, &cell_ptr_offsets, &cells);

        let mut overflow_page = vec![0u8; PAGE_SIZE];
        overflow_page[0..4].copy_from_slice(&overflow_next.to_be_bytes());
        let copy_len = overflow_body.len().min(PAGE_SIZE - 4);
        overflow_page[4..4 + copy_len].copy_from_slice(&overflow_body[..copy_len]);

        let mut pages = HashMap::new();
        pages.insert(2u32, leaf);
        pages.insert(3u32, overflow_page);
        let source = FakePageSource { pages };

        drive_cursor(source, 2);
    }
}
