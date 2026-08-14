//! Table b-tree read path (Tier 0): a read-only cursor over table b-trees
//! (page types 0x05 interior / 0x0d leaf), including overflow-chain
//! reassembly. See `.openspec/specs/006-btree/spec.md` for the page/cell
//! byte layout this module implements.
//!
//! Page-1 trap: page 1 carries the 100-byte file header before its own
//! b-tree page header, but its cell-pointer array is still relative to
//! byte 0 of the page (see `src/header.rs`'s module doc). Every page-1
//! read in this module resolves cell offsets from page start, not from
//! `header_start`.
//!
//! Rowid-alias note: a column declared exactly `INTEGER PRIMARY KEY` is
//! not stored in the record — SQLite encodes it as NULL and expects the
//! reader to substitute the cell's own rowid. This module returns the
//! record payload faithfully (NULL and all); substituting the alias
//! column is a schema-aware operation that belongs above this layer,
//! once the DDL reader (step 7) knows which column, if any, is the alias.

mod error;
mod index;

pub use error::BtreeError;
pub use index::{IndexCursor, IndexRow};

use crate::header::DatabaseHeader;
use crate::record::decode_varint;
use crate::vfs::PageSource;

const LEAF_TABLE: u8 = 0x0d;
const INTERIOR_TABLE: u8 = 0x05;

/// SQLite's documented maximum size for a single value (2^31 - 1 bytes) —
/// used to reject implausible `payload_len` claims before attempting an
/// allocation.
const MAX_PAYLOAD_LEN: u64 = 2_147_483_647;

/// Sanity cap on total pages visited by a single cursor traversal or
/// overflow-chain walk, guarding against a corrupt/cyclic file causing an
/// unbounded loop.
const MAX_PAGES_VISITED: usize = 1_000_000;

/// One decoded table b-tree row: the SQLite rowid and its raw record
/// payload (after overflow-chain reassembly). See the module doc's
/// rowid-alias note — `payload` may encode a rowid-alias column as NULL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRow {
    pub rowid: i64,
    pub payload: Vec<u8>,
}

struct Frame {
    page_num: u32,
    is_interior: bool,
    num_cells: usize,
    cell_ptr_base: usize,
    next_cell: usize,
    rightmost: u32,
    rightmost_done: bool,
    page: Vec<u8>,
}

/// Depth-first, read-only cursor over a table b-tree, yielding rows in
/// ascending rowid order.
pub struct TableCursor<P: PageSource> {
    source: P,
    usable_size: u32,
    root_page: u32,
    stack: Vec<Frame>,
    pages_visited: usize,
}

impl<P: PageSource> TableCursor<P> {
    pub fn new(source: P, header: &DatabaseHeader, root_page: u32) -> Self {
        TableCursor {
            source,
            usable_size: header.usable_page_size(),
            root_page,
            stack: Vec::new(),
            pages_visited: 0,
        }
    }

    /// Positions the cursor at the first row and returns it, or `None` if
    /// the table is empty. Resets any prior traversal position.
    pub fn first(&mut self) -> Result<Option<TableRow>, BtreeError> {
        self.stack.clear();
        self.pages_visited = 0;
        self.push_page(self.root_page)?;
        self.advance()
    }

    /// Advances to the next row and returns it, or `None` once exhausted.
    /// Call [`Self::first`] first; calling `next` before `first` behaves
    /// as an empty cursor.
    #[allow(clippy::should_implement_trait)] // deliberate cursor API (first/next/seek), not std::iter::Iterator
    pub fn next(&mut self) -> Result<Option<TableRow>, BtreeError> {
        self.advance()
    }

    /// Looks up the row with exactly `target_rowid`, independent of the
    /// `first`/`next` traversal position. Returns `None` if no such row
    /// exists.
    pub fn seek(&mut self, target_rowid: i64) -> Result<Option<TableRow>, BtreeError> {
        let mut page_num = self.root_page;
        let mut visited = 0usize;
        loop {
            visited += 1;
            if visited > MAX_PAGES_VISITED {
                return Err(BtreeError::TraversalTooLong {
                    max: MAX_PAGES_VISITED,
                });
            }
            let page = self.read_page(page_num)?;
            let header_start = page1_header_start(page_num);
            let page_type = read_page_type(&page, header_start, page_num)?;
            let num_cells = read_num_cells(&page, header_start, page_num)?;

            match page_type {
                LEAF_TABLE => {
                    let cell_ptr_base = header_start + 8;
                    for i in 0..num_cells {
                        let cell_start =
                            read_cell_pointer(&page, cell_ptr_base + 2 * i, page_num, i)?;
                        let (rowid, payload_len, tail_start) =
                            decode_cell_head(&page, cell_start, page_num)?;
                        if rowid == target_rowid {
                            let tail = page
                                .get(tail_start..)
                                .ok_or(BtreeError::PayloadTooShort { page_num })?;
                            let payload = reassemble_payload(
                                &self.source,
                                self.usable_size,
                                page_num,
                                tail,
                                payload_len,
                            )?;
                            return Ok(Some(TableRow { rowid, payload }));
                        }
                    }
                    return Ok(None);
                }
                INTERIOR_TABLE => {
                    require_interior_header(&page, header_start, page_num)?;
                    let cell_ptr_base = header_start + 12;
                    let rightmost = read_u32(&page, header_start + 8, page_num)?;
                    let mut next_page = rightmost;
                    for i in 0..num_cells {
                        let cell_start =
                            read_cell_pointer(&page, cell_ptr_base + 2 * i, page_num, i)?;
                        let child = read_u32(&page, cell_start, page_num)?;
                        let key_bytes = page
                            .get(cell_start + 4..)
                            .ok_or(BtreeError::InvalidCellPointer { page_num, index: i })?;
                        let (key, _) = decode_varint(key_bytes)
                            .map_err(|source| BtreeError::InvalidCellVarint { page_num, source })?;
                        if target_rowid <= key as i64 {
                            next_page = child;
                            break;
                        }
                    }
                    page_num = next_page;
                }
                other => {
                    return Err(BtreeError::UnexpectedPageType {
                        page_num,
                        page_type: other,
                    })
                }
            }
        }
    }

    fn read_page(&mut self, page_num: u32) -> Result<Vec<u8>, BtreeError> {
        self.pages_visited += 1;
        if self.pages_visited > MAX_PAGES_VISITED {
            return Err(BtreeError::TraversalTooLong {
                max: MAX_PAGES_VISITED,
            });
        }
        self.source
            .read_page(page_num)
            .map_err(|source| BtreeError::PageSource { page_num, source })
    }

    fn push_page(&mut self, page_num: u32) -> Result<(), BtreeError> {
        let page = self.read_page(page_num)?;
        let header_start = page1_header_start(page_num);
        let page_type = read_page_type(&page, header_start, page_num)?;
        let is_interior = match page_type {
            LEAF_TABLE => false,
            INTERIOR_TABLE => true,
            other => {
                return Err(BtreeError::UnexpectedPageType {
                    page_num,
                    page_type: other,
                })
            }
        };
        if is_interior {
            require_interior_header(&page, header_start, page_num)?;
        }
        let num_cells = read_num_cells(&page, header_start, page_num)?;
        let (cell_ptr_base, rightmost) = if is_interior {
            (
                header_start + 12,
                read_u32(&page, header_start + 8, page_num)?,
            )
        } else {
            (header_start + 8, 0)
        };
        self.stack.push(Frame {
            page_num,
            is_interior,
            num_cells,
            cell_ptr_base,
            next_cell: 0,
            rightmost,
            rightmost_done: !is_interior,
            page,
        });
        Ok(())
    }

    fn advance(&mut self) -> Result<Option<TableRow>, BtreeError> {
        loop {
            let top = match self.stack.len() {
                0 => return Ok(None),
                n => n - 1,
            };
            let (is_interior, next_cell, num_cells, rightmost, rightmost_done) = {
                let f = &self.stack[top];
                (
                    f.is_interior,
                    f.next_cell,
                    f.num_cells,
                    f.rightmost,
                    f.rightmost_done,
                )
            };

            if !is_interior {
                if next_cell >= num_cells {
                    self.stack.pop();
                    continue;
                }
                self.stack[top].next_cell += 1;
                return self.decode_leaf_cell(top, next_cell).map(Some);
            }

            if next_cell < num_cells {
                self.stack[top].next_cell += 1;
                let child = self.read_interior_child(top, next_cell)?;
                self.push_page(child)?;
            } else if !rightmost_done {
                self.stack[top].rightmost_done = true;
                self.push_page(rightmost)?;
            } else {
                self.stack.pop();
            }
        }
    }

    fn read_interior_child(
        &self,
        frame_index: usize,
        cell_index: usize,
    ) -> Result<u32, BtreeError> {
        let frame = &self.stack[frame_index];
        let ptr_off = frame.cell_ptr_base + 2 * cell_index;
        let cell_start = read_cell_pointer(&frame.page, ptr_off, frame.page_num, cell_index)?;
        read_u32(&frame.page, cell_start, frame.page_num)
    }

    fn decode_leaf_cell(
        &self,
        frame_index: usize,
        cell_index: usize,
    ) -> Result<TableRow, BtreeError> {
        let frame = &self.stack[frame_index];
        let page_num = frame.page_num;
        let ptr_off = frame.cell_ptr_base + 2 * cell_index;
        let cell_start = read_cell_pointer(&frame.page, ptr_off, page_num, cell_index)?;
        let (rowid, payload_len, tail_start) = decode_cell_head(&frame.page, cell_start, page_num)?;
        let tail = frame
            .page
            .get(tail_start..)
            .ok_or(BtreeError::PayloadTooShort { page_num })?;
        let payload =
            reassemble_payload(&self.source, self.usable_size, page_num, tail, payload_len)?;
        Ok(TableRow { rowid, payload })
    }
}

/// SQLite's overflow local-size formula (fileformat2.html "Cell Payload
/// Overflow"), shared by table leaf cells, index leaf cells, and index
/// interior cells (table interior cells have no payload at all). All
/// arithmetic saturates rather than panics — a pathological `usable_size`
/// degrades to a safe (wrong but non-panicking) answer, caught by the
/// length checks around the call site instead of an arithmetic panic
/// here.
fn local_payload_size(usable_size: u32, payload_len: u64) -> u64 {
    let max_local = usable_size.saturating_sub(35) as u64;
    if payload_len <= max_local {
        return payload_len;
    }
    let min_local = ((usable_size.saturating_sub(12) as u64 * 32) / 255).saturating_sub(23);
    let denom = usable_size.saturating_sub(4).max(1) as u64;
    let k = min_local + payload_len.saturating_sub(min_local) % denom;
    if k <= max_local {
        k
    } else {
        min_local
    }
}

fn reassemble_payload<P: PageSource>(
    source: &P,
    usable_size: u32,
    page_num: u32,
    cell_tail: &[u8],
    payload_len: u64,
) -> Result<Vec<u8>, BtreeError> {
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(BtreeError::PayloadTooLarge {
            page_num,
            payload_len,
        });
    }
    let local_size = local_payload_size(usable_size, payload_len) as usize;
    let local_bytes = cell_tail
        .get(..local_size)
        .ok_or(BtreeError::PayloadTooShort { page_num })?;
    if local_size as u64 == payload_len {
        return Ok(local_bytes.to_vec());
    }

    let overflow_ptr = cell_tail
        .get(local_size..local_size + 4)
        .ok_or(BtreeError::PayloadTooShort { page_num })?;
    let mut overflow_page = u32::from_be_bytes(overflow_ptr.try_into().unwrap());
    let mut remaining = payload_len - local_size as u64;
    let mut result = local_bytes.to_vec();
    let available = usable_size.saturating_sub(4).max(1) as u64;
    let mut hops = 0usize;

    while remaining > 0 {
        if overflow_page == 0 {
            return Err(BtreeError::OverflowChainTruncated { page_num });
        }
        hops += 1;
        if hops > MAX_PAGES_VISITED {
            return Err(BtreeError::OverflowChainTooLong {
                page_num,
                max: MAX_PAGES_VISITED,
            });
        }
        let page = source
            .read_page(overflow_page)
            .map_err(|source| BtreeError::PageSource {
                page_num: overflow_page,
                source,
            })?;
        let next = read_u32(&page, 0, overflow_page)?;
        let take = remaining.min(available) as usize;
        let chunk = page.get(4..4 + take).ok_or(BtreeError::PageTooShort {
            page_num: overflow_page,
            len: page.len(),
        })?;
        result.extend_from_slice(chunk);
        remaining -= take as u64;
        overflow_page = next;
    }
    Ok(result)
}

fn page1_header_start(page_num: u32) -> usize {
    if page_num == 1 {
        100
    } else {
        0
    }
}

fn read_page_type(page: &[u8], header_start: usize, page_num: u32) -> Result<u8, BtreeError> {
    page.get(header_start)
        .copied()
        .ok_or(BtreeError::PageTooShort {
            page_num,
            len: page.len(),
        })
}

fn read_num_cells(page: &[u8], header_start: usize, page_num: u32) -> Result<usize, BtreeError> {
    let bytes = page
        .get(header_start + 3..header_start + 5)
        .ok_or(BtreeError::PageTooShort {
            page_num,
            len: page.len(),
        })?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]) as usize)
}

fn require_interior_header(
    page: &[u8],
    header_start: usize,
    page_num: u32,
) -> Result<(), BtreeError> {
    if page.len() < header_start + 12 {
        return Err(BtreeError::PageTooShort {
            page_num,
            len: page.len(),
        });
    }
    Ok(())
}

fn read_u32(page: &[u8], offset: usize, page_num: u32) -> Result<u32, BtreeError> {
    let bytes = page
        .get(offset..offset + 4)
        .ok_or(BtreeError::PageTooShort {
            page_num,
            len: page.len(),
        })?;
    Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
}

fn read_cell_pointer(
    page: &[u8],
    ptr_off: usize,
    page_num: u32,
    cell_index: usize,
) -> Result<usize, BtreeError> {
    let bytes = page
        .get(ptr_off..ptr_off + 2)
        .ok_or(BtreeError::InvalidCellPointer {
            page_num,
            index: cell_index,
        })?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]) as usize)
}

/// Decodes a leaf table-b-tree cell's head (payload-length varint + rowid
/// varint) and returns `(rowid, payload_len, tail_start)`, where
/// `tail_start` is the page offset where the payload bytes begin.
fn decode_cell_head(
    page: &[u8],
    cell_start: usize,
    page_num: u32,
) -> Result<(i64, u64, usize), BtreeError> {
    let cell = page
        .get(cell_start..)
        .ok_or(BtreeError::InvalidCellPointer {
            page_num,
            index: cell_start,
        })?;
    let (payload_len, n1) =
        decode_varint(cell).map_err(|source| BtreeError::InvalidCellVarint { page_num, source })?;
    let rest = cell
        .get(n1..)
        .ok_or(BtreeError::PayloadTooShort { page_num })?;
    let (rowid, n2) =
        decode_varint(rest).map_err(|source| BtreeError::InvalidCellVarint { page_num, source })?;
    Ok((rowid as i64, payload_len, cell_start + n1 + n2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::DatabaseHeader;
    use crate::record::{decode_record, TextEncoding, Value};
    use crate::vfs::{PageError, UnixVfs, Vfs, VfsPageSource};
    use std::collections::HashMap;
    use std::path::Path;

    fn open_cursor(fixture: &str) -> TableCursor<VfsPageSource> {
        let path = Path::new("tests/corpus/fixtures/btrees").join(fixture);
        let vfs = UnixVfs;
        let file = vfs.open_read(&path).unwrap();
        let mut header_buf = [0u8; 100];
        file.read_at(&mut header_buf, 0).unwrap();
        let header = DatabaseHeader::parse(&header_buf).unwrap();
        let source = VfsPageSource::open(&vfs, &path, header.page_size).unwrap();
        TableCursor::new(source, &header, 2)
    }

    fn text(v: &Value) -> &str {
        match v {
            Value::Text(s) => s,
            other => panic!("expected text, got {other:?}"),
        }
    }

    fn int(v: &Value) -> i64 {
        match v {
            Value::Integer(i) => *i,
            other => panic!("expected integer, got {other:?}"),
        }
    }

    fn blob(v: &Value) -> &[u8] {
        match v {
            Value::Blob(b) => b,
            other => panic!("expected blob, got {other:?}"),
        }
    }

    #[test]
    fn table_single_page_full_scan() {
        let mut cursor = open_cursor("table_single_page.db");
        let row = cursor.first().unwrap().unwrap();
        assert_eq!(row.rowid, 1);
        let values = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
        assert_eq!(int(&values[0]), 1);
        assert_eq!(text(&values[1]), "a single leaf page");
        assert!(cursor.next().unwrap().is_none());
    }

    #[test]
    fn table_multipage_full_scan_matches_oracle() {
        let mut cursor = open_cursor("table_multipage.db");
        let mut rows = Vec::new();
        let mut row = cursor.first().unwrap();
        while let Some(r) = row {
            rows.push(r);
            row = cursor.next().unwrap();
        }
        assert_eq!(rows.len(), 3000);

        // Ascending rowid order, 1..=3000, no gaps or duplicates.
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r.rowid, (i + 1) as i64);
        }

        let first = decode_record(&rows[0].payload, TextEncoding::Utf8).unwrap();
        assert_eq!(int(&first[0]), 1);
        assert_eq!(text(&first[1]), "row number 1");

        let last = decode_record(&rows[2999].payload, TextEncoding::Utf8).unwrap();
        assert_eq!(int(&last[0]), 3000);
        assert_eq!(text(&last[1]), "row number 3000");
    }

    #[test]
    fn page_one_trap_sqlite_master_root_is_page_one() {
        // Page 1 carries the 100-byte file header before its own b-tree
        // page header; this reads sqlite_master (always root page 1)
        // directly, exercising the page-1 cell-pointer-array offset
        // resolution (relative to byte 0, not byte 100).
        let path = Path::new("tests/corpus/fixtures/btrees/table_single_page.db");
        let vfs = UnixVfs;
        let file = vfs.open_read(path).unwrap();
        let mut header_buf = [0u8; 100];
        file.read_at(&mut header_buf, 0).unwrap();
        let header = DatabaseHeader::parse(&header_buf).unwrap();
        let source = VfsPageSource::open(&vfs, path, header.page_size).unwrap();
        let mut cursor = TableCursor::new(source, &header, 1);

        let row = cursor.first().unwrap().unwrap();
        let values = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
        assert_eq!(text(&values[0]), "table");
        assert_eq!(text(&values[1]), "t");
        assert_eq!(text(&values[2]), "t");
        assert_eq!(int(&values[3]), 2);
        assert_eq!(text(&values[4]), "CREATE TABLE t(a INTEGER, b TEXT)");
        assert!(cursor.next().unwrap().is_none());
    }

    #[test]
    fn table_multipage_seek_matches_oracle() {
        let mut cursor = open_cursor("table_multipage.db");

        let row = cursor.seek(1500).unwrap().unwrap();
        assert_eq!(row.rowid, 1500);
        let values = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
        assert_eq!(text(&values[1]), "row number 1500");

        let first = cursor.seek(1).unwrap().unwrap();
        assert_eq!(first.rowid, 1);
        let last = cursor.seek(3000).unwrap().unwrap();
        assert_eq!(last.rowid, 3000);

        assert!(cursor.seek(0).unwrap().is_none());
        assert!(cursor.seek(3001).unwrap().is_none());
    }

    #[test]
    fn overflow_single_page_payload_is_byte_identical_to_oracle() {
        let mut cursor = open_cursor("overflow_single_page.db");
        let row = cursor.first().unwrap().unwrap();
        let values = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
        assert_eq!(int(&values[0]), 1);
        let b = blob(&values[1]);
        assert_eq!(b.len(), 6000);
        assert_eq!(
            sha256_of(b),
            "a6bedce1e512d6531cd02fe7a0b72bb64f229cdb254ec48d63308877004e620a"
        );
        assert!(cursor.next().unwrap().is_none());
    }

    #[test]
    fn overflow_multi_page_payload_is_byte_identical_to_oracle() {
        let mut cursor = open_cursor("overflow_multi_page.db");
        let row = cursor.first().unwrap().unwrap();
        let values = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
        assert_eq!(int(&values[0]), 1);
        let b = blob(&values[1]);
        assert_eq!(b.len(), 60000);
        assert_eq!(
            sha256_of(b),
            "0946e2eb0fb9ea7ddd935efd1922bc7d1f27101c69ce6d2f5145c7ee28f1b6ba"
        );
        assert!(cursor.next().unwrap().is_none());
    }

    /// SHA-256, implemented locally (no new dependency) purely to verify
    /// overflow-chain reassembly is byte-identical to the oracle without
    /// embedding 60000 bytes of expected data in the test source.
    fn sha256_of(data: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];

        let mut msg = data.to_vec();
        let bit_len = (data.len() as u64) * 8;
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());

        for chunk in msg.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }

            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let temp1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let temp2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(temp1);
                d = c;
                c = b;
                b = a;
                a = temp1.wrapping_add(temp2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }

        h.iter().map(|x| format!("{x:08x}")).collect()
    }

    struct FakePageSource {
        pages: HashMap<u32, Vec<u8>>,
    }

    impl PageSource for FakePageSource {
        fn read_page(&self, page_num: u32) -> Result<Vec<u8>, PageError> {
            self.pages
                .get(&page_num)
                .cloned()
                .ok_or(PageError::InvalidPageNumber)
        }
    }

    fn fake_header() -> DatabaseHeader {
        DatabaseHeader {
            page_size: 512,
            write_version: 1,
            read_version: 1,
            reserved_space: 0,
            page_count: 1,
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

    #[test]
    fn unexpected_page_type_errors_not_panics() {
        let mut page = vec![0u8; 512];
        page[0] = 0xff; // not a valid table b-tree page type
        let mut pages = HashMap::new();
        pages.insert(2u32, page);
        let source = FakePageSource { pages };
        let mut cursor = TableCursor::new(source, &fake_header(), 2);

        let err = cursor.first().unwrap_err();
        assert!(matches!(
            err,
            BtreeError::UnexpectedPageType {
                page_num: 2,
                page_type: 0xff
            }
        ));
    }

    #[test]
    fn truncated_page_errors_not_panics() {
        let mut pages = HashMap::new();
        pages.insert(2u32, vec![0x0d, 0, 0]); // page type + 2 bytes, way short of an 8-byte header
        let source = FakePageSource { pages };
        let mut cursor = TableCursor::new(source, &fake_header(), 2);

        let err = cursor.first().unwrap_err();
        assert!(matches!(err, BtreeError::PageTooShort { page_num: 2, .. }));
    }

    #[test]
    fn missing_page_errors_not_panics() {
        let source = FakePageSource {
            pages: HashMap::new(),
        };
        let mut cursor = TableCursor::new(source, &fake_header(), 2);

        let err = cursor.first().unwrap_err();
        assert!(matches!(err, BtreeError::PageSource { page_num: 2, .. }));
    }

    #[test]
    fn overflow_chain_hitting_page_zero_early_errors_not_panics() {
        // A leaf page with one cell whose declared payload_len is larger
        // than what's actually reachable: local bytes + a next-overflow
        // pointer of 0 (chain end) while `remaining` is still nonzero.
        let mut page = vec![0u8; 512];
        page[0] = 0x0d; // leaf table
        page[3..5].copy_from_slice(&1u16.to_be_bytes()); // num_cells = 1
        let cell_ptr_off = 8usize;
        let cell_start = 16usize;
        page[cell_ptr_off..cell_ptr_off + 2].copy_from_slice(&(cell_start as u16).to_be_bytes());

        // payload_len varint = 500 (way past max_local for a 512-byte
        // usable page), rowid varint = 1, then local bytes + a 4-byte
        // overflow pointer of 0.
        let mut cell = Vec::new();
        cell.extend_from_slice(&encode_varint_for_test(500));
        cell.extend_from_slice(&encode_varint_for_test(1));
        let local_size_guess = 512usize.saturating_sub(35).min(470); // generous local region
        cell.extend(std::iter::repeat_n(0u8, local_size_guess));
        cell.extend_from_slice(&0u32.to_be_bytes()); // overflow pointer = 0 (chain end)
        page[cell_start..cell_start + cell.len()].copy_from_slice(&cell);

        let mut pages = HashMap::new();
        pages.insert(2u32, page);
        let source = FakePageSource { pages };
        let mut cursor = TableCursor::new(source, &fake_header(), 2);

        let err = cursor.first().unwrap_err();
        assert!(matches!(
            err,
            BtreeError::OverflowChainTruncated { page_num: 2 }
        ));
    }

    fn encode_varint_for_test(mut value: u64) -> Vec<u8> {
        // Minimal single/double-byte varint encoder sufficient for small
        // test values (this crate's real decoder handles the full 9-byte
        // form; this helper only needs to round-trip through it).
        if value < 0x80 {
            return vec![value as u8];
        }
        let mut bytes = Vec::new();
        let mut chunks = Vec::new();
        loop {
            chunks.push((value & 0x7f) as u8);
            value >>= 7;
            if value == 0 {
                break;
            }
        }
        chunks.reverse();
        for (i, c) in chunks.iter().enumerate() {
            if i + 1 == chunks.len() {
                bytes.push(*c);
            } else {
                bytes.push(c | 0x80);
            }
        }
        bytes
    }
}
