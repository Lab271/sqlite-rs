//! Table b-tree insert (write path): cell insert, leaf split, cascading
//! interior splits, and root split. See
//! `.openspec/specs/006-btree/spec.md` (insert/split requirements) for the
//! byte-layout contract this module writes. Read-side helpers (`page1_header_start`,
//! `read_page_type`, `read_num_cells`, `read_u32`, `read_cell_pointer`,
//! `cell_ptr_offset`, `decode_cell_head`, `local_payload_size`) are reused
//! directly from the parent `btree` module — they're private to `btree` but
//! visible here as a descendant module.
//!
//! Simplifications made for this ticket (documented rather than hidden):
//! every page mutation here fully rebuilds the page's cell-pointer array
//! and content area from scratch (no freeblock/fragmented-byte reuse), and
//! reserved-space-per-page (`usable_size < page_size`) is not accounted for
//! in the rebuild — both match this codebase's current at-rest fixtures
//! (reserved bytes always 0) but would need generalizing before a `PRAGMA
//! reserve_bytes` fixture is supported.

use super::{
    build_interior_cell, collect_interior_entries, collect_leaf_cells, find_leaf_page,
    local_payload_size, page1_header_start, put, read_page_type, write_interior_page,
    write_leaf_page, BtreeError, INTERIOR_TABLE, LEAF_TABLE,
};
use crate::header::DatabaseHeader;
use crate::pager::Pager;
use crate::record::encode_varint;

/// Inserts one row `(rowid, payload)` into the table b-tree rooted at
/// `root_page`, splitting leaves/interior pages (and the root itself) as
/// needed to make room. `payload` is the already record-encoded row body
/// (e.g. from [`crate::record::encode_record`]) — this function does not
/// re-encode it, only frames it into a b-tree cell.
pub fn insert_row(
    pager: &mut Pager,
    header: &DatabaseHeader,
    root_page: u32,
    rowid: i64,
    payload: &[u8],
) -> Result<(), BtreeError> {
    let usable_size = header.usable_page_size();
    let cell = encode_leaf_cell(pager, usable_size, rowid, payload)?;
    let (ancestors, leaf_page) = find_leaf_page(pager, root_page, rowid)?;
    let page_len = pager.get_page_mut(leaf_page)?.len();
    insert_into_leaf(
        pager,
        usable_size,
        page_len,
        leaf_page,
        root_page,
        &ancestors,
        rowid,
        cell,
    )
}

/// Builds a leaf table-b-tree cell: payload-length varint + rowid varint +
/// local payload bytes, plus a trailing 4-byte overflow-page pointer when
/// `payload` doesn't fit locally (fileformat2.html "Cell Payload Overflow").
fn encode_leaf_cell(
    pager: &mut Pager,
    usable_size: u32,
    rowid: i64,
    payload: &[u8],
) -> Result<Vec<u8>, BtreeError> {
    let payload_len = payload.len() as u64;
    let local_size = (local_payload_size(usable_size, payload_len) as usize).min(payload.len());
    let (local_bytes, overflow_bytes) = payload.split_at(local_size);
    let mut cell = encode_varint(payload_len);
    cell.extend(encode_varint(rowid as u64));
    cell.extend_from_slice(local_bytes);
    if !overflow_bytes.is_empty() {
        let overflow_first = write_overflow_chain(pager, usable_size, overflow_bytes)?;
        cell.extend_from_slice(&overflow_first.to_be_bytes());
    }
    Ok(cell)
}

/// Writes `data` across freshly allocated overflow pages (each: 4-byte
/// next-page-number + chunk bytes, 0 terminates), mirroring
/// `reassemble_payload`'s read-side chain format in reverse. Returns the
/// first overflow page number.
fn write_overflow_chain(
    pager: &mut Pager,
    usable_size: u32,
    data: &[u8],
) -> Result<u32, BtreeError> {
    let available = usable_size.saturating_sub(4).max(1) as usize;
    let mut chunks: Vec<&[u8]> = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let take = rest.len().min(available);
        let (chunk, tail) = rest.split_at(take);
        chunks.push(chunk);
        rest = tail;
    }

    let mut page_nums = Vec::with_capacity(chunks.len());
    for _ in &chunks {
        page_nums.push(pager.allocate_page()?);
    }
    for (i, chunk) in chunks.iter().enumerate() {
        let next = page_nums.get(i.saturating_add(1)).copied().unwrap_or(0);
        let page_num = *page_nums.get(i).ok_or(BtreeError::Internal(
            "overflow chain page index out of bounds",
        ))?;
        let buf = pager.get_page_mut(page_num)?;
        put(buf, 0, &next.to_be_bytes(), page_num)?;
        put(buf, 4, chunk, page_num)?;
    }
    Ok(page_nums.first().copied().unwrap_or(0))
}

/// Inserts `cell` (already encoded, for `rowid`) into leaf page
/// `leaf_page`, splitting it (and cascading into ancestors/`root_page`) if
/// it doesn't fit.
#[allow(clippy::too_many_arguments)]
fn insert_into_leaf(
    pager: &mut Pager,
    usable_size: u32,
    page_len: usize,
    leaf_page: u32,
    root_page: u32,
    ancestors: &[u32],
    rowid: i64,
    cell: Vec<u8>,
) -> Result<(), BtreeError> {
    let header_start = page1_header_start(leaf_page);
    let buf = pager.get_page_mut(leaf_page)?.clone();
    let mut cells = collect_leaf_cells(&buf, header_start, leaf_page, usable_size)?;

    let mut insert_pos = cells.len();
    for (i, (existing_rowid, _)) in cells.iter().enumerate() {
        if *existing_rowid == rowid {
            return Err(BtreeError::DuplicateRowid { rowid });
        }
        if *existing_rowid > rowid {
            insert_pos = i;
            break;
        }
    }
    cells.insert(insert_pos, (rowid, cell));

    let total_bytes: usize = cells.iter().map(|(_, c)| c.len()).sum();
    let header_len = 8;
    let needed = header_start
        .saturating_add(header_len)
        .saturating_add(cells.len().saturating_mul(2))
        .saturating_add(total_bytes);
    if needed <= page_len {
        let buf = pager.get_page_mut(leaf_page)?;
        write_leaf_page(
            buf,
            header_start,
            leaf_page,
            &cells.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
        )?;
        return Ok(());
    }

    // Split: left keeps the lower half (including here if inserted there),
    // right (a freshly allocated page) takes the upper half.
    let n = cells.len();
    let left_n = n.div_ceil(2);
    let right_page = pager.allocate_page()?;
    let right = cells.split_off(left_n);
    let left = cells;
    let divider = left
        .last()
        .ok_or(BtreeError::Internal(
            "left half of a split leaf must not be empty",
        ))?
        .0;

    {
        let buf = pager.get_page_mut(leaf_page)?;
        write_leaf_page(
            buf,
            header_start,
            leaf_page,
            &left.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
        )?;
    }
    {
        let buf = pager.get_page_mut(right_page)?;
        write_leaf_page(
            buf,
            0,
            right_page,
            &right.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
        )?;
    }

    insert_into_parent(
        pager,
        usable_size,
        page_len,
        ancestors,
        root_page,
        leaf_page,
        right_page,
        divider,
    )
}

/// Propagates a child split (`old_page` keeps its identity as the left
/// sibling, `new_page` is the freshly allocated right sibling, `divider` is
/// the max key routed to `old_page`) into its parent, splitting the parent
/// (or the root itself) if needed.
#[allow(clippy::too_many_arguments)]
fn insert_into_parent(
    pager: &mut Pager,
    usable_size: u32,
    page_len: usize,
    ancestors: &[u32],
    root_page: u32,
    old_page: u32,
    new_page: u32,
    divider: i64,
) -> Result<(), BtreeError> {
    let Some((&parent_page, rest)) = ancestors.split_last() else {
        return root_split(pager, usable_size, root_page, new_page, divider);
    };

    let header_start = page1_header_start(parent_page);
    let buf = pager.get_page_mut(parent_page)?.clone();
    let (mut entries, mut rightmost) = collect_interior_entries(&buf, header_start, parent_page)?;

    match entries.iter().position(|(child, _)| *child == old_page) {
        Some(idx) => {
            entries.insert(idx, (old_page, divider));
            let successor = entries
                .get_mut(idx.saturating_add(1))
                .ok_or(BtreeError::Internal(
                    "split successor entry must exist right after insertion",
                ))?;
            successor.0 = new_page;
        }
        None if rightmost == old_page => {
            entries.push((old_page, divider));
            rightmost = new_page;
        }
        None => {
            return Err(BtreeError::MissingChildRoute {
                page_num: parent_page,
                child: old_page,
            });
        }
    }

    let cell_bytes: Vec<Vec<u8>> = entries
        .iter()
        .map(|(child, key)| build_interior_cell(*child, *key))
        .collect();
    let total_bytes: usize = cell_bytes.iter().map(Vec::len).sum();
    let header_len = 12;
    let needed = header_start
        .saturating_add(header_len)
        .saturating_add(cell_bytes.len().saturating_mul(2))
        .saturating_add(total_bytes);
    if needed <= page_len {
        let buf = pager.get_page_mut(parent_page)?;
        write_interior_page(buf, header_start, parent_page, &cell_bytes, rightmost)?;
        return Ok(());
    }

    // Interior split: the median key is promoted to the grandparent
    // without being duplicated in either child.
    let n = entries.len();
    let mid = n / 2;
    let (promoted_child, promoted_key) = *entries.get(mid).ok_or(BtreeError::Internal(
        "median entry index out of bounds during interior split",
    ))?;
    let left_entries = entries.get(..mid).ok_or(BtreeError::Internal(
        "left interior split range out of bounds",
    ))?;
    let right_entries = entries
        .get(mid.saturating_add(1)..)
        .ok_or(BtreeError::Internal(
            "right interior split range out of bounds",
        ))?;

    let right_page_num = pager.allocate_page()?;
    {
        let cells: Vec<Vec<u8>> = left_entries
            .iter()
            .map(|(child, key)| build_interior_cell(*child, *key))
            .collect();
        let buf = pager.get_page_mut(parent_page)?;
        write_interior_page(buf, header_start, parent_page, &cells, promoted_child)?;
    }
    {
        let cells: Vec<Vec<u8>> = right_entries
            .iter()
            .map(|(child, key)| build_interior_cell(*child, *key))
            .collect();
        let buf = pager.get_page_mut(right_page_num)?;
        write_interior_page(buf, 0, right_page_num, &cells, rightmost)?;
    }

    insert_into_parent(
        pager,
        usable_size,
        page_len,
        rest,
        root_page,
        parent_page,
        right_page_num,
        promoted_key,
    )
}

/// The root page number can never change (schema entries point at it), so
/// a root split relocates the root's current content (leaf or interior)
/// verbatim to a freshly allocated page, then reinitializes the root
/// page in-place as a new interior page with one cell pointing at the
/// relocated content and `new_right` as the rightmost pointer.
fn root_split(
    pager: &mut Pager,
    usable_size: u32,
    root_page: u32,
    new_right: u32,
    divider: i64,
) -> Result<(), BtreeError> {
    let header_start_root = page1_header_start(root_page);
    let content = pager.get_page_mut(root_page)?.clone();
    let page_type = read_page_type(&content, header_start_root, root_page)?;
    let relocated = pager.allocate_page()?;

    match page_type {
        LEAF_TABLE => {
            let cells = collect_leaf_cells(&content, header_start_root, root_page, usable_size)?;
            let dest = pager.get_page_mut(relocated)?;
            write_leaf_page(
                dest,
                0,
                relocated,
                &cells.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
            )?;
        }
        INTERIOR_TABLE => {
            let (entries, rightmost) =
                collect_interior_entries(&content, header_start_root, root_page)?;
            let cells: Vec<Vec<u8>> = entries
                .iter()
                .map(|(child, key)| build_interior_cell(*child, *key))
                .collect();
            let dest = pager.get_page_mut(relocated)?;
            write_interior_page(dest, 0, relocated, &cells, rightmost)?;
        }
        other => {
            return Err(BtreeError::UnexpectedPageType {
                page_num: root_page,
                page_type: other,
            })
        }
    }

    let cell = build_interior_cell(relocated, divider);
    let buf = pager.get_page_mut(root_page)?;
    write_interior_page(buf, header_start_root, root_page, &[cell], new_right)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::vfs::MemoryVfs;
    use std::path::Path;

    /// A one-page, empty-leaf-root database: just enough header bytes for
    /// `DatabaseHeader::parse` and `Pager::open` to accept it.
    fn minimal_db(page_size: u32) -> (MemoryVfs, DatabaseHeader) {
        let mut page1 = vec![0u8; page_size as usize];
        page1[0..16].copy_from_slice(b"SQLite format 3\0");
        page1[16..18].copy_from_slice(&(page_size as u16).to_be_bytes());
        page1[18] = 1;
        page1[19] = 1;
        page1[28..32].copy_from_slice(&1u32.to_be_bytes());
        page1[56..60].copy_from_slice(&1u32.to_be_bytes());
        write_leaf_page(&mut page1, 100, 1, &[]).unwrap();

        let mut header_bytes = [0u8; 100];
        header_bytes.copy_from_slice(&page1[..100]);
        let header = DatabaseHeader::parse(&header_bytes).unwrap();

        let mut vfs = MemoryVfs::new();
        vfs.insert("/test.db", page1);
        (vfs, header)
    }

    /// 006-btree Requirement 8's duplicate-rowid scenario: inserting a
    /// rowid that already exists in the leaf must error, not silently
    /// overwrite or duplicate the row.
    #[test]
    fn duplicate_rowid_is_rejected() {
        let page_size = 512u32;
        let (vfs, header) = minimal_db(page_size);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();

        insert_row(&mut pager, &header, 1, 1, b"hello").unwrap();
        let err = insert_row(&mut pager, &header, 1, 1, b"world").unwrap_err();
        assert!(matches!(err, BtreeError::DuplicateRowid { rowid: 1 }));
    }
}
