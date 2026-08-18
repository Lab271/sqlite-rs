//! Index b-tree insert (write path): entry insert, leaf split, cascading
//! interior splits, and root split. WITHOUT ROWID tables are index
//! b-trees (see `index.rs`'s module doc) — the same `insert_entry` writes
//! both ordinary secondary indexes and WITHOUT ROWID table storage.
//!
//! Structurally this mirrors `insert.rs` (table insert), with one real
//! difference: index b-tree interior cells carry a **full entry**, not
//! just a routing key (per `index.rs`'s module doc), so index leaf split
//! behaves like `insert.rs::insert_into_parent`'s *interior*-split branch
//! — the median entry is promoted (removed) into the parent rather than
//! copied as a separator and kept in the leaf. Leaf-level and
//! interior-level splits are therefore the same shape here, unlike the
//! table write path where leaf split (copy-and-keep divider) and interior
//! split (promote-and-remove) differ.
//!
//! Position/ordering uses [`super::index::compare_keys`] (BINARY-collation
//! key comparison) rather than numeric rowid comparison. Shares the same
//! "every page mutation fully rebuilds the page" simplification as
//! `insert.rs`.

use super::index::{
    build_index_interior_cell, collect_index_interior_entries, collect_index_leaf_cells,
    compare_keys, descend_index_tree, write_index_interior_page, write_index_leaf_page,
    IndexDescent, INTERIOR_INDEX, LEAF_INDEX,
};
use super::{local_payload_size, page1_header_start, put, read_page_type, BtreeError};
use crate::header::DatabaseHeader;
use crate::pager::Pager;
use crate::record::{encode_record, encode_varint, TextEncoding, Value};

/// Inserts one entry (`key`, a full record — indexed columns plus the
/// referenced rowid for an ordinary secondary index, or the whole row for
/// a WITHOUT ROWID table) into the index b-tree rooted at `root_page`,
/// splitting leaves/interior pages (and the root itself) as needed.
/// Returns `Err(BtreeError::DuplicateKey)` if an entry comparing exactly
/// equal to `key` (via [`compare_keys`]) already exists.
pub fn insert_entry(
    pager: &mut Pager,
    header: &DatabaseHeader,
    root_page: u32,
    key: &[Value],
    encoding: TextEncoding,
) -> Result<(), BtreeError> {
    let usable_size = header.usable_page_size();
    let payload = encode_record(key, encoding);
    let cell = encode_index_cell(pager, usable_size, &payload)?;
    let (ancestors, leaf_page) =
        match descend_index_tree(pager, root_page, usable_size, key, encoding)? {
            IndexDescent::Leaf {
                ancestors,
                leaf_page,
            } => (ancestors, leaf_page),
            IndexDescent::InteriorMatch { .. } => return Err(BtreeError::DuplicateKey),
        };
    let page_len = pager.get_page_mut(leaf_page)?.len();
    insert_into_index_leaf(
        pager,
        usable_size,
        page_len,
        leaf_page,
        root_page,
        &ancestors,
        key,
        cell,
        encoding,
    )
}

/// Builds an index leaf/interior "value cell": payload-length varint +
/// local payload bytes, plus a trailing 4-byte overflow-page pointer when
/// `payload` doesn't fit locally — the same shape as a table leaf cell
/// minus the rowid varint (index cells carry the rowid as an embedded
/// record column instead, per `index.rs`'s module doc).
fn encode_index_cell(
    pager: &mut Pager,
    usable_size: u32,
    payload: &[u8],
) -> Result<Vec<u8>, BtreeError> {
    let payload_len = payload.len() as u64;
    let local_size = (local_payload_size(usable_size, payload_len) as usize).min(payload.len());
    let (local_bytes, overflow_bytes) = payload.split_at(local_size);
    let mut cell = encode_varint(payload_len);
    cell.extend_from_slice(local_bytes);
    if !overflow_bytes.is_empty() {
        let overflow_first = write_overflow_chain(pager, usable_size, overflow_bytes)?;
        cell.extend_from_slice(&overflow_first.to_be_bytes());
    }
    Ok(cell)
}

/// Writes `data` across freshly allocated overflow pages, mirroring
/// `insert.rs::write_overflow_chain` (duplicated rather than shared: a
/// generic over two near-identical one-line-different callers isn't
/// worth the indirection here).
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

/// Inserts `cell` (already encoded, for `key`) into leaf page `leaf_page`,
/// splitting it (and cascading into ancestors/`root_page`) if it doesn't
/// fit. Unlike a table leaf split, the split's median entry is promoted
/// (removed from both halves) into the parent — see the module doc.
#[allow(clippy::too_many_arguments)]
fn insert_into_index_leaf(
    pager: &mut Pager,
    usable_size: u32,
    page_len: usize,
    leaf_page: u32,
    root_page: u32,
    ancestors: &[u32],
    key: &[Value],
    cell: Vec<u8>,
    encoding: TextEncoding,
) -> Result<(), BtreeError> {
    let header_start = page1_header_start(leaf_page);
    let buf = pager.get_page_mut(leaf_page)?.clone();
    let mut cells =
        collect_index_leaf_cells(pager, &buf, header_start, leaf_page, usable_size, encoding)?;

    let mut insert_pos = cells.len();
    for (i, (existing_key, _)) in cells.iter().enumerate() {
        match compare_keys(key, existing_key) {
            std::cmp::Ordering::Equal => return Err(BtreeError::DuplicateKey),
            std::cmp::Ordering::Less => {
                insert_pos = i;
                break;
            }
            std::cmp::Ordering::Greater => {}
        }
    }
    cells.insert(insert_pos, (key.to_vec(), cell));

    let total_bytes: usize = cells.iter().map(|(_, c)| c.len()).sum();
    let header_len = 8;
    let needed = header_start
        .saturating_add(header_len)
        .saturating_add(cells.len().saturating_mul(2))
        .saturating_add(total_bytes);
    if needed <= page_len {
        let buf = pager.get_page_mut(leaf_page)?;
        write_index_leaf_page(
            buf,
            header_start,
            leaf_page,
            &cells.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
        )?;
        return Ok(());
    }

    // Split: the median entry is promoted into the parent (removed from
    // both halves); left keeps entries less than it, right (a freshly
    // allocated page) keeps entries greater.
    let n = cells.len();
    let mid = n / 2;
    let (promoted_key, promoted_bytes) = cells.get(mid).cloned().ok_or(BtreeError::Internal(
        "median entry index out of bounds during index leaf split",
    ))?;
    let right_page = pager.allocate_page()?;
    let right = cells.split_off(mid.saturating_add(1));
    cells.truncate(mid);
    let left = cells;

    {
        let buf = pager.get_page_mut(leaf_page)?;
        write_index_leaf_page(
            buf,
            header_start,
            leaf_page,
            &left.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
        )?;
    }
    {
        let buf = pager.get_page_mut(right_page)?;
        write_index_leaf_page(
            buf,
            0,
            right_page,
            &right.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
        )?;
    }

    insert_into_index_parent(
        pager,
        usable_size,
        page_len,
        ancestors,
        root_page,
        leaf_page,
        right_page,
        &promoted_key,
        promoted_bytes,
        encoding,
    )
}

/// Propagates a child split (`old_page` keeps its identity as the left
/// sibling, `new_page` is the freshly allocated right sibling,
/// `promoted_key`/`promoted_bytes` is the entry promoted from the child)
/// into its parent, splitting the parent (or the root itself) if needed.
#[allow(clippy::too_many_arguments)]
fn insert_into_index_parent(
    pager: &mut Pager,
    usable_size: u32,
    page_len: usize,
    ancestors: &[u32],
    root_page: u32,
    old_page: u32,
    new_page: u32,
    promoted_key: &[Value],
    promoted_bytes: Vec<u8>,
    encoding: TextEncoding,
) -> Result<(), BtreeError> {
    let Some((&parent_page, rest)) = ancestors.split_last() else {
        return root_split(
            pager,
            usable_size,
            root_page,
            new_page,
            promoted_bytes,
            encoding,
        );
    };

    let header_start = page1_header_start(parent_page);
    let buf = pager.get_page_mut(parent_page)?.clone();
    let (mut entries, mut rightmost) = collect_index_interior_entries(
        pager,
        &buf,
        header_start,
        parent_page,
        usable_size,
        encoding,
    )?;

    match entries.iter().position(|(child, _, _)| *child == old_page) {
        Some(idx) => {
            entries.insert(idx, (old_page, promoted_key.to_vec(), promoted_bytes));
            let successor = entries
                .get_mut(idx.saturating_add(1))
                .ok_or(BtreeError::Internal(
                    "split successor entry must exist right after insertion",
                ))?;
            successor.0 = new_page;
        }
        None if rightmost == old_page => {
            entries.push((old_page, promoted_key.to_vec(), promoted_bytes));
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
        .map(|(child, _, value_bytes)| build_index_interior_cell(*child, value_bytes))
        .collect();
    let total_bytes: usize = cell_bytes.iter().map(Vec::len).sum();
    let header_len = 12;
    let needed = header_start
        .saturating_add(header_len)
        .saturating_add(cell_bytes.len().saturating_mul(2))
        .saturating_add(total_bytes);
    if needed <= page_len {
        let buf = pager.get_page_mut(parent_page)?;
        write_index_interior_page(buf, header_start, parent_page, &cell_bytes, rightmost)?;
        return Ok(());
    }

    // Interior split: same promote-and-remove shape as the leaf split.
    let n = entries.len();
    let mid = n / 2;
    let (promoted_child, promoted_key, promoted_bytes) = entries.get(mid).cloned().ok_or(
        BtreeError::Internal("median entry index out of bounds during index interior split"),
    )?;
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
            .map(|(child, _, value_bytes)| build_index_interior_cell(*child, value_bytes))
            .collect();
        let buf = pager.get_page_mut(parent_page)?;
        write_index_interior_page(buf, header_start, parent_page, &cells, promoted_child)?;
    }
    {
        let cells: Vec<Vec<u8>> = right_entries
            .iter()
            .map(|(child, _, value_bytes)| build_index_interior_cell(*child, value_bytes))
            .collect();
        let buf = pager.get_page_mut(right_page_num)?;
        write_index_interior_page(buf, 0, right_page_num, &cells, rightmost)?;
    }

    insert_into_index_parent(
        pager,
        usable_size,
        page_len,
        rest,
        root_page,
        parent_page,
        right_page_num,
        &promoted_key,
        promoted_bytes,
        encoding,
    )
}

/// The root page number can never change, so an index root split
/// relocates the root's current content (leaf or interior, verbatim) to a
/// freshly allocated page, then reinitializes the root page in-place as a
/// new interior page holding one cell (the promoted entry, routing to the
/// relocated page) and `new_right` as the rightmost pointer. Mirrors
/// `insert.rs::root_split`.
fn root_split(
    pager: &mut Pager,
    usable_size: u32,
    root_page: u32,
    new_right: u32,
    promoted_bytes: Vec<u8>,
    encoding: TextEncoding,
) -> Result<(), BtreeError> {
    let header_start_root = page1_header_start(root_page);
    let content = pager.get_page_mut(root_page)?.clone();
    let page_type = read_page_type(&content, header_start_root, root_page)?;
    let relocated = pager.allocate_page()?;

    match page_type {
        LEAF_INDEX => {
            let cells = collect_index_leaf_cells(
                pager,
                &content,
                header_start_root,
                root_page,
                usable_size,
                encoding,
            )?;
            let dest = pager.get_page_mut(relocated)?;
            write_index_leaf_page(
                dest,
                0,
                relocated,
                &cells.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
            )?;
        }
        INTERIOR_INDEX => {
            let (entries, rightmost) = collect_index_interior_entries(
                pager,
                &content,
                header_start_root,
                root_page,
                usable_size,
                encoding,
            )?;
            let cells: Vec<Vec<u8>> = entries
                .iter()
                .map(|(child, _, value_bytes)| build_index_interior_cell(*child, value_bytes))
                .collect();
            let dest = pager.get_page_mut(relocated)?;
            write_index_interior_page(dest, 0, relocated, &cells, rightmost)?;
        }
        other => {
            return Err(BtreeError::UnexpectedPageType {
                page_num: root_page,
                page_type: other,
            })
        }
    }

    let cell = build_index_interior_cell(relocated, &promoted_bytes);
    let buf = pager.get_page_mut(root_page)?;
    write_index_interior_page(buf, header_start_root, root_page, &[cell], new_right)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use crate::vfs::MemoryVfs;
    use std::path::Path;

    /// A one-page, empty-leaf-root database whose root is an index leaf
    /// (`LEAF_INDEX`) instead of a table leaf.
    fn minimal_index_db(page_size: u32) -> (MemoryVfs, DatabaseHeader) {
        let mut page1 = vec![0u8; page_size as usize];
        page1[0..16].copy_from_slice(b"SQLite format 3\0");
        page1[16..18].copy_from_slice(&(page_size as u16).to_be_bytes());
        page1[18] = 1;
        page1[19] = 1;
        page1[28..32].copy_from_slice(&1u32.to_be_bytes());
        page1[56..60].copy_from_slice(&1u32.to_be_bytes());
        write_index_leaf_page(&mut page1, 100, 1, &[]).unwrap();

        let mut header_bytes = [0u8; 100];
        header_bytes.copy_from_slice(&page1[..100]);
        let header = DatabaseHeader::parse(&header_bytes).unwrap();

        let mut vfs = MemoryVfs::new();
        vfs.insert("/test.db", page1);
        (vfs, header)
    }

    fn key(a: &str, rowid: i64) -> Vec<Value> {
        vec![Value::Text(a.to_string()), Value::Integer(rowid)]
    }

    #[test]
    fn duplicate_key_is_rejected() {
        let page_size = 512u32;
        let (vfs, header) = minimal_index_db(page_size);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();

        insert_entry(&mut pager, &header, 1, &key("a", 1), TextEncoding::Utf8).unwrap();
        let err =
            insert_entry(&mut pager, &header, 1, &key("a", 1), TextEncoding::Utf8).unwrap_err();
        assert!(matches!(err, BtreeError::DuplicateKey));
    }

    #[test]
    fn entries_stay_in_ascending_key_order() {
        let page_size = 512u32;
        let (vfs, header) = minimal_index_db(page_size);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();

        insert_entry(
            &mut pager,
            &header,
            1,
            &key("banana", 2),
            TextEncoding::Utf8,
        )
        .unwrap();
        insert_entry(&mut pager, &header, 1, &key("apple", 1), TextEncoding::Utf8).unwrap();
        insert_entry(
            &mut pager,
            &header,
            1,
            &key("cherry", 3),
            TextEncoding::Utf8,
        )
        .unwrap();

        let header_start = page1_header_start(1);
        let buf = pager.get_page_mut(1).unwrap().clone();
        let cells = collect_index_leaf_cells(
            &pager,
            &buf,
            header_start,
            1,
            header.usable_page_size(),
            TextEncoding::Utf8,
        )
        .unwrap();
        assert_eq!(cells.len(), 3);
        let texts: Vec<&str> = cells
            .iter()
            .map(|(k, _)| match &k[0] {
                Value::Text(s) => s.as_str(),
                _ => panic!("expected text"),
            })
            .collect();
        assert_eq!(texts, vec!["apple", "banana", "cherry"]);
    }
}
