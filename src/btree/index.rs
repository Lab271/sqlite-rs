//! Index b-tree read path (Tier 0): a read-only cursor over index b-trees
//! (page types 0x02 interior / 0x0a leaf). WITHOUT ROWID tables are
//! stored as index b-trees — confirmed on a real fixture (spike 005,
//! #12: FTS5's `t_idx`/`t_config` shadow tables) — so this cursor is what
//! makes those tables (and ordinary secondary indexes) readable at all.
//!
//! Unlike table b-trees, index b-tree **interior cells carry a full key
//! payload**, not just a routing pointer: an interior cell represents a
//! real, sorted entry (with a left-child subtree of lesser keys), not
//! merely a separator. In-order traversal therefore yields each interior
//! cell's own key interleaved with descending into its children —
//! different from `TableCursor`'s traversal, which never yields data
//! from an interior page.
//!
//! Key comparison (NULL < numeric < text < blob, BINARY collation only —
//! Tier 0 scope) is minimal by design, per the originating issue: enough
//! ordering to walk in the correct sequence, not a fully general seek.
//! [`IndexCursor::seek`] is a linear scan from the first entry rather
//! than a tree descent, trading O(log n) for a much simpler, harder-to-
//! get-wrong implementation — acceptable at Tier 0 scope.

use std::cmp::Ordering;

use super::{
    cell_ptr_offset, page1_header_start, read_cell_pointer, read_num_cells, read_page_type,
    read_u32, reassemble_payload, require_interior_header, BtreeError, MAX_PAGES_VISITED,
};
use crate::record::{decode_record, decode_varint, TextEncoding, Value};
use crate::vfs::PageSource;

const LEAF_INDEX: u8 = 0x0a;
const INTERIOR_INDEX: u8 = 0x02;

/// One decoded index b-tree entry: the raw key-record payload (after
/// overflow-chain reassembly). For an ordinary secondary index the
/// decoded record's last column is the referenced table's rowid; for a
/// WITHOUT ROWID table's own storage the decoded record IS the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRow {
    pub payload: Vec<u8>,
}

struct IndexFrame {
    page_num: u32,
    is_interior: bool,
    num_cells: usize,
    cell_ptr_base: usize,
    rightmost: u32,
    /// Leaf: cell index to yield next, 0..num_cells.
    /// Interior: a step counter over `2 * num_cells + 1` steps — even
    /// step `2*i` descends into cell `i`'s left child, odd step `2*i+1`
    /// yields cell `i`'s own key, and the final step `2*num_cells`
    /// descends into the rightmost child.
    step: usize,
    page: Vec<u8>,
}

/// Depth-first, read-only cursor over an index b-tree, yielding entries
/// in ascending key order (BINARY collation).
pub struct IndexCursor<P: PageSource> {
    source: P,
    usable_size: u32,
    root_page: u32,
    stack: Vec<IndexFrame>,
    pages_visited: usize,
}

impl<P: PageSource> IndexCursor<P> {
    pub fn new(source: P, usable_size: u32, root_page: u32) -> Self {
        IndexCursor {
            source,
            usable_size,
            root_page,
            stack: Vec::new(),
            pages_visited: 0,
        }
    }

    /// Positions the cursor at the first entry and returns it, or `None`
    /// if the index is empty. Resets any prior traversal position.
    pub fn first(&mut self) -> Result<Option<IndexRow>, BtreeError> {
        self.stack.clear();
        self.pages_visited = 0;
        self.push_page(self.root_page)?;
        self.advance()
    }

    /// Advances to the next entry and returns it, or `None` once
    /// exhausted. Call [`Self::first`] first.
    #[allow(clippy::should_implement_trait)] // deliberate cursor API, not std::iter::Iterator
    pub fn next(&mut self) -> Result<Option<IndexRow>, BtreeError> {
        self.advance()
    }

    /// Returns the first entry (in ascending key order) whose decoded key
    /// is not less than `target`, or `None` if every entry is less than
    /// `target`. A linear scan from the first entry — see the module doc
    /// for why that's an intentional Tier 0 simplification.
    pub fn seek(
        &mut self,
        target: &[Value],
        encoding: TextEncoding,
    ) -> Result<Option<IndexRow>, BtreeError> {
        let mut row = self.first()?;
        while let Some(r) = row {
            let key = decode_record(&r.payload, encoding)?;
            if compare_keys(&key, target) != Ordering::Less {
                return Ok(Some(r));
            }
            row = self.next()?;
        }
        Ok(None)
    }

    fn read_page(&mut self, page_num: u32) -> Result<Vec<u8>, BtreeError> {
        self.pages_visited = self.pages_visited.saturating_add(1);
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
            LEAF_INDEX => false,
            INTERIOR_INDEX => true,
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
                header_start.saturating_add(12),
                read_u32(&page, header_start.saturating_add(8), page_num)?,
            )
        } else {
            (header_start.saturating_add(8), 0)
        };
        self.stack.push(IndexFrame {
            page_num,
            is_interior,
            num_cells,
            cell_ptr_base,
            rightmost,
            step: 0,
            page,
        });
        Ok(())
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "top = stack.len() - 1, computed just above from a non-empty check; always in bounds"
    )]
    fn advance(&mut self) -> Result<Option<IndexRow>, BtreeError> {
        loop {
            let top = match self.stack.len() {
                0 => return Ok(None),
                n => n.saturating_sub(1),
            };
            let (is_interior, step, num_cells, rightmost) = {
                let f = &self.stack[top];
                (f.is_interior, f.step, f.num_cells, f.rightmost)
            };

            if !is_interior {
                if step >= num_cells {
                    self.stack.pop();
                    continue;
                }
                self.stack[top].step = self.stack[top].step.saturating_add(1);
                return self.decode_leaf_entry(top, step).map(Some);
            }

            let total_steps = num_cells.saturating_mul(2);
            if step > total_steps {
                self.stack.pop();
                continue;
            }
            self.stack[top].step = self.stack[top].step.saturating_add(1);
            if step == total_steps {
                self.push_page(rightmost)?;
            } else if step % 2 == 0 {
                let child = self.read_interior_child(top, step / 2)?;
                self.push_page(child)?;
            } else {
                // step is odd here (the step % 2 == 0 arm above didn't
                // match), so step >= 1 and this never underflows.
                return self
                    .decode_interior_entry(top, step.saturating_sub(1) / 2)
                    .map(Some);
            }
        }
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "frame_index is always `top` from advance(), always in bounds"
    )]
    fn read_interior_child(
        &self,
        frame_index: usize,
        cell_index: usize,
    ) -> Result<u32, BtreeError> {
        let frame = &self.stack[frame_index];
        let ptr_off = cell_ptr_offset(frame.cell_ptr_base, cell_index);
        let cell_start = read_cell_pointer(&frame.page, ptr_off, frame.page_num, cell_index)?;
        read_u32(&frame.page, cell_start, frame.page_num)
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "frame_index is always `top` from advance(), always in bounds"
    )]
    fn decode_leaf_entry(
        &self,
        frame_index: usize,
        cell_index: usize,
    ) -> Result<IndexRow, BtreeError> {
        let frame = &self.stack[frame_index];
        let page_num = frame.page_num;
        let ptr_off = cell_ptr_offset(frame.cell_ptr_base, cell_index);
        let cell_start = read_cell_pointer(&frame.page, ptr_off, page_num, cell_index)?;
        let (payload_len, tail_start) = decode_payload_len(&frame.page, cell_start, page_num)?;
        let tail = frame
            .page
            .get(tail_start..)
            .ok_or(BtreeError::PayloadTooShort { page_num })?;
        let payload =
            reassemble_payload(&self.source, self.usable_size, page_num, tail, payload_len)?;
        Ok(IndexRow { payload })
    }

    #[allow(
        clippy::indexing_slicing,
        reason = "frame_index is always `top` from advance(), always in bounds"
    )]
    fn decode_interior_entry(
        &self,
        frame_index: usize,
        cell_index: usize,
    ) -> Result<IndexRow, BtreeError> {
        let frame = &self.stack[frame_index];
        let page_num = frame.page_num;
        let ptr_off = cell_ptr_offset(frame.cell_ptr_base, cell_index);
        let cell_start = read_cell_pointer(&frame.page, ptr_off, page_num, cell_index)?;
        // Interior index cell: 4-byte left-child pointer, then the same
        // payload-length-varint + payload shape as a leaf cell.
        let (payload_len, tail_start) =
            decode_payload_len(&frame.page, cell_start.saturating_add(4), page_num)?;
        let tail = frame
            .page
            .get(tail_start..)
            .ok_or(BtreeError::PayloadTooShort { page_num })?;
        let payload =
            reassemble_payload(&self.source, self.usable_size, page_num, tail, payload_len)?;
        Ok(IndexRow { payload })
    }
}

fn decode_payload_len(
    page: &[u8],
    offset: usize,
    page_num: u32,
) -> Result<(u64, usize), BtreeError> {
    let cell = page.get(offset..).ok_or(BtreeError::InvalidCellPointer {
        page_num,
        index: offset,
    })?;
    let (payload_len, n1) =
        decode_varint(cell).map_err(|source| BtreeError::InvalidCellVarint { page_num, source })?;
    Ok((payload_len, offset.saturating_add(n1)))
}

/// SQLite's Tier 0 (BINARY collation) type ordering: NULL < numeric <
/// text < blob.
fn value_rank(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Integer(_) | Value::Real(_) => 1,
        Value::Text(_) => 2,
        Value::Blob(_) => 3,
    }
}

fn compare_values(a: &Value, b: &Value) -> Ordering {
    let (ra, rb) = (value_rank(a), value_rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
        (Value::Real(x), Value::Real(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Integer(x), Value::Real(y)) => {
            (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal)
        }
        (Value::Real(x), Value::Integer(y)) => {
            x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal)
        }
        (Value::Text(x), Value::Text(y)) => x.as_bytes().cmp(y.as_bytes()),
        (Value::Blob(x), Value::Blob(y)) => x.cmp(y),
        _ => Ordering::Equal, // unreachable: value_rank already separated these
    }
}

/// Lexicographic key comparison over a (possibly composite) index key.
fn compare_keys(a: &[Value], b: &[Value]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let c = compare_values(x, y);
        if c != Ordering::Equal {
            return c;
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects
)]
mod tests {
    use super::*;
    use crate::header::DatabaseHeader;
    use crate::vfs::{PageError, UnixVfs, Vfs, VfsPageSource};
    use std::collections::HashMap;
    use std::path::Path;

    fn open_cursor(fixture: &str, root_page: u32) -> IndexCursor<VfsPageSource> {
        let path = Path::new("tests/corpus/fixtures/btrees").join(fixture);
        let vfs = UnixVfs;
        let file = vfs.open_read(&path).unwrap();
        let mut header_buf = [0u8; 100];
        file.read_at(&mut header_buf, 0).unwrap();
        let header = DatabaseHeader::parse(&header_buf).unwrap();
        let source = VfsPageSource::open(&vfs, &path, header.page_size).unwrap();
        IndexCursor::new(source, header.usable_page_size(), root_page)
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

    #[test]
    fn secondary_index_walk_matches_oracle_binary_order() {
        // idx_b on t(b); ascending b, BINARY collation (lexicographic,
        // not numeric) — "row number 1" < "row number 10" < "row number
        // 100" < "row number 1000" < "row number 1001", confirmed against
        // `sqlite3 ... ORDER BY b, a`.
        let mut cursor = open_cursor("index.db", 3);
        let mut rows = Vec::new();
        let mut row = cursor.first().unwrap();
        while let Some(r) = row {
            rows.push(r);
            row = cursor.next().unwrap();
        }
        assert_eq!(rows.len(), 3000);

        let expect = [
            ("row number 1", 1i64),
            ("row number 10", 10),
            ("row number 100", 100),
            ("row number 1000", 1000),
            ("row number 1001", 1001),
        ];
        for (row, (b, a)) in rows.iter().zip(expect.iter()) {
            let key = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
            assert_eq!(text(&key[0]), *b);
            assert_eq!(int(&key[1]), *a);
        }

        // Strictly ascending BINARY order end to end (walks the full
        // interior+leaf structure correctly, not just the first page).
        for i in 1..rows.len() {
            let prev = decode_record(&rows[i - 1].payload, TextEncoding::Utf8).unwrap();
            let cur = decode_record(&rows[i].payload, TextEncoding::Utf8).unwrap();
            assert_ne!(compare_keys(&prev, &cur), Ordering::Greater);
        }
    }

    #[test]
    fn secondary_index_seek_matches_oracle() {
        let mut cursor = open_cursor("index.db", 3);
        let target = [Value::Text("row number 100".to_string())];
        let row = cursor.seek(&target, TextEncoding::Utf8).unwrap().unwrap();
        let key = decode_record(&row.payload, TextEncoding::Utf8).unwrap();
        assert_eq!(text(&key[0]), "row number 100");
        assert_eq!(int(&key[1]), 100);
    }

    #[test]
    fn without_rowid_table_is_readable_as_index_btree() {
        // t(k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID — the table's own
        // storage IS an index b-tree keyed on k; the decoded record is
        // the full row, not a separate key+rowid split.
        let mut cursor = open_cursor("without_rowid.db", 2);
        let mut rows = Vec::new();
        let mut row = cursor.first().unwrap();
        while let Some(r) = row {
            rows.push(r);
            row = cursor.next().unwrap();
        }
        assert_eq!(rows.len(), 500);

        let first = decode_record(&rows[0].payload, TextEncoding::Utf8).unwrap();
        assert_eq!(text(&first[0]), "key1");
        assert_eq!(text(&first[1]), "value number 1");

        // BINARY collation: "key1" < "key10" < "key100" < "key99"
        // (shorter-is-less on a shared prefix), confirmed against oracle.
        let expect_order = ["key1", "key10", "key100", "key99"];
        let mut idx = 0;
        for r in &rows {
            let key = decode_record(&r.payload, TextEncoding::Utf8).unwrap();
            if idx < expect_order.len() && text(&key[0]) == expect_order[idx] {
                idx += 1;
            }
        }
        assert_eq!(idx, expect_order.len(), "expected keys not seen in order");

        for i in 1..rows.len() {
            let prev = decode_record(&rows[i - 1].payload, TextEncoding::Utf8).unwrap();
            let cur = decode_record(&rows[i].payload, TextEncoding::Utf8).unwrap();
            assert_ne!(compare_keys(&prev, &cur), Ordering::Greater);
        }
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

    #[test]
    fn unexpected_page_type_errors_not_panics() {
        let mut page = vec![0u8; 512];
        page[0] = 0xff; // not a valid index b-tree page type
        let mut pages = HashMap::new();
        pages.insert(2u32, page);
        let source = FakePageSource { pages };
        let mut cursor = IndexCursor::new(source, 512, 2);

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
        pages.insert(2u32, vec![0x0a, 0, 0]); // leaf index type + 2 bytes, short of an 8-byte header
        let source = FakePageSource { pages };
        let mut cursor = IndexCursor::new(source, 512, 2);

        let err = cursor.first().unwrap_err();
        assert!(matches!(err, BtreeError::PageTooShort { page_num: 2, .. }));
    }
}
