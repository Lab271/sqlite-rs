//! Table b-tree delete (write path): cell delete plus page
//! merge/collapse on underflow. See `.openspec/specs/006-btree/spec.md`
//! (delete/collapse requirements) for the byte-layout contract this
//! module writes. Read-side and shared write-path helpers
//! (`page1_header_start`, `read_page_type`, `find_leaf_page`,
//! `collect_leaf_cells`, `collect_interior_entries`, `build_interior_cell`,
//! `write_leaf_page`, `write_interior_page`) are reused directly from the
//! parent `btree` module — see `insert.rs`'s module doc for the "every
//! page mutation fully rebuilds the page" simplification they share.
//!
//! Underflow policy (this ticket's "page merge/rebalance" scope item):
//! rather than porting SQLite's exact 3-sibling balance algorithm (which
//! proactively redistributes/merges pages once they drop below a
//! half-full threshold), this module only collapses a page when a delete
//! leaves it **completely empty**. An empty leaf is removed from its
//! parent and deallocated (returned to the freelist, #167); if that
//! leaves the parent itself with zero routing entries (just its
//! `rightmost` pointer), the parent is collapsed the same way, cascading
//! up to the root if necessary. This keeps every page a delete leaves
//! behind non-empty and structurally valid — sufficient for
//! `PRAGMA integrity_check` and for freed pages to be reused by a later
//! insert via the freelist — without implementing proactive half-full
//! redistribution, which nothing else in this codebase's rebuild-from-
//! scratch write path does either.

use super::{
    build_interior_cell, collect_interior_entries, collect_leaf_cells, decode_cell_head,
    find_leaf_page, local_payload_size, page1_header_start, read_page_type, read_u32,
    write_interior_page, write_leaf_page, BtreeError, INTERIOR_TABLE, LEAF_TABLE,
};
use crate::header::DatabaseHeader;
use crate::pager::Pager;

/// Deletes the row with `rowid` from the table b-tree rooted at
/// `root_page`, collapsing an emptied leaf (and cascading into ancestors,
/// up to `root_page` itself) as needed. Returns
/// `Err(BtreeError::RowidNotFound)` if no such row exists, leaving the
/// tree unchanged.
pub fn delete_row(
    pager: &mut Pager,
    header: &DatabaseHeader,
    root_page: u32,
    rowid: i64,
) -> Result<(), BtreeError> {
    let usable_size = header.usable_page_size();
    let (ancestors, leaf_page) = find_leaf_page(pager, root_page, rowid)?;

    let header_start = page1_header_start(leaf_page);
    let buf = pager.get_page_mut(leaf_page)?.clone();
    let mut cells = collect_leaf_cells(&buf, header_start, leaf_page, usable_size)?;

    let pos = cells
        .iter()
        .position(|(existing_rowid, _)| *existing_rowid == rowid)
        .ok_or(BtreeError::RowidNotFound { rowid })?;
    let (_, removed_cell) = cells.remove(pos);
    let overflow_page = cell_overflow_page(&removed_cell, leaf_page, usable_size)?;

    let remaining: Vec<Vec<u8>> = cells.into_iter().map(|(_, c)| c).collect();
    if !remaining.is_empty() || ancestors.is_empty() {
        // Either the leaf still holds rows, or it's the root itself (which
        // can't be removed/collapsed — an empty root leaf is a valid,
        // empty table).
        let buf = pager.get_page_mut(leaf_page)?;
        write_leaf_page(buf, header_start, leaf_page, &remaining)?;
        return free_overflow_chain(pager, overflow_page);
    }

    let buf = pager.get_page_mut(leaf_page)?;
    write_leaf_page(buf, header_start, leaf_page, &remaining)?;
    pager.deallocate_page(leaf_page)?;
    free_overflow_chain(pager, overflow_page)?;
    collapse_into_ancestors(pager, usable_size, root_page, &ancestors, leaf_page)
}

/// Returns the first overflow page referenced by a leaf cell (as produced
/// by [`collect_leaf_cells`]), or `0` if the cell's payload fit entirely
/// locally. `collect_leaf_cells` already trims each cell to exactly its
/// local bytes plus (when present) the trailing 4-byte overflow pointer,
/// so the pointer — when present — is always the cell's last 4 bytes.
fn cell_overflow_page(cell: &[u8], page_num: u32, usable_size: u32) -> Result<u32, BtreeError> {
    let (_, payload_len, tail_start) = decode_cell_head(cell, 0, page_num)?;
    let local_size = local_payload_size(usable_size, payload_len) as usize;
    if (local_size as u64) >= payload_len {
        return Ok(0);
    }
    let ptr_start = tail_start.saturating_add(local_size);
    read_u32(cell, ptr_start, page_num)
}

/// Walks an overflow chain starting at `first_page` (a no-op if it's `0`,
/// i.e. the deleted cell had no overflow), deallocating every page in the
/// chain back to the freelist (#167). Guards against a corrupt/cyclic
/// chain the same way [`super::reassemble_payload`] does, so a malformed
/// on-disk chain fails deletion cleanly rather than looping forever.
fn free_overflow_chain(pager: &mut Pager, first_page: u32) -> Result<(), BtreeError> {
    let mut page_num = first_page;
    let mut visited = std::collections::HashSet::new();
    while page_num != 0 {
        if !visited.insert(page_num) {
            return Err(BtreeError::OverflowChainCycle {
                page_num: first_page,
                revisited_page: page_num,
            });
        }
        let buf = pager.get_page_mut(page_num)?.clone();
        let next = read_u32(&buf, 0, page_num)?;
        pager.deallocate_page(page_num)?;
        page_num = next;
    }
    Ok(())
}

/// Removes the routing entry for `emptied_page` from its immediate parent
/// (the last entry in `ancestors`), then — if that leaves the parent with
/// no routing entries at all (down to just its `rightmost` pointer) —
/// recursively collapses the parent into its own parent (or, if the
/// parent is the root, relocates the sole remaining child's content into
/// the root page in place, mirroring `insert.rs::root_split` in reverse).
fn collapse_into_ancestors(
    pager: &mut Pager,
    usable_size: u32,
    root_page: u32,
    ancestors: &[u32],
    emptied_page: u32,
) -> Result<(), BtreeError> {
    let Some((&parent_page, rest)) = ancestors.split_last() else {
        return Err(BtreeError::Internal(
            "collapse_into_ancestors called with no ancestors",
        ));
    };

    let header_start = page1_header_start(parent_page);
    let buf = pager.get_page_mut(parent_page)?.clone();
    let (mut entries, mut rightmost) = collect_interior_entries(&buf, header_start, parent_page)?;

    match entries.iter().position(|(child, _)| *child == emptied_page) {
        Some(idx) => {
            entries.remove(idx);
        }
        None if rightmost == emptied_page => {
            let Some((last_child, _)) = entries.pop() else {
                return Err(BtreeError::Internal(
                    "interior page's rightmost pointer was emptied but it has no routing entries to promote",
                ));
            };
            rightmost = last_child;
        }
        None => {
            return Err(BtreeError::MissingChildRoute {
                page_num: parent_page,
                child: emptied_page,
            });
        }
    }

    if !entries.is_empty() {
        let cell_bytes: Vec<Vec<u8>> = entries
            .iter()
            .map(|(child, key)| build_interior_cell(*child, *key))
            .collect();
        let buf = pager.get_page_mut(parent_page)?;
        return write_interior_page(buf, header_start, parent_page, &cell_bytes, rightmost);
    }

    // The parent now has zero routing entries — its only remaining child
    // is `rightmost`. It no longer earns its own page: collapse it away.
    if parent_page == root_page {
        return collapse_root(pager, usable_size, root_page, rightmost);
    }

    let buf = pager.get_page_mut(parent_page)?;
    write_interior_page(buf, header_start, parent_page, &[], rightmost)?;
    pager.deallocate_page(parent_page)?;
    collapse_into_ancestors(pager, usable_size, root_page, rest, parent_page)
}

/// The root page number can never change, so collapsing the root's sole
/// remaining child (`only_child`) means relocating that child's content
/// (leaf or interior, verbatim) into the root page in place, then
/// deallocating `only_child`'s now-vacated page. Mirrors
/// `insert.rs::root_split` in reverse.
fn collapse_root(
    pager: &mut Pager,
    usable_size: u32,
    root_page: u32,
    only_child: u32,
) -> Result<(), BtreeError> {
    let child_header_start = page1_header_start(only_child);
    let content = pager.get_page_mut(only_child)?.clone();
    let page_type = read_page_type(&content, child_header_start, only_child)?;
    let root_header_start = page1_header_start(root_page);

    match page_type {
        LEAF_TABLE => {
            let cells = collect_leaf_cells(&content, child_header_start, only_child, usable_size)?;
            let dest = pager.get_page_mut(root_page)?;
            write_leaf_page(
                dest,
                root_header_start,
                root_page,
                &cells.into_iter().map(|(_, c)| c).collect::<Vec<_>>(),
            )?;
        }
        INTERIOR_TABLE => {
            let (entries, rightmost) =
                collect_interior_entries(&content, child_header_start, only_child)?;
            let cells: Vec<Vec<u8>> = entries
                .iter()
                .map(|(child, key)| build_interior_cell(*child, *key))
                .collect();
            let dest = pager.get_page_mut(root_page)?;
            write_interior_page(dest, root_header_start, root_page, &cells, rightmost)?;
        }
        other => {
            return Err(BtreeError::UnexpectedPageType {
                page_num: only_child,
                page_type: other,
            })
        }
    }

    Ok(pager.deallocate_page(only_child)?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::btree::insert_row;
    use crate::vfs::MemoryVfs;
    use std::path::Path;

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

    #[test]
    fn deleting_a_missing_rowid_errors() {
        let page_size = 512u32;
        let (vfs, header) = minimal_db(page_size);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();

        insert_row(&mut pager, &header, 1, 1, b"hello").unwrap();
        let err = delete_row(&mut pager, &header, 1, 2).unwrap_err();
        assert!(matches!(err, BtreeError::RowidNotFound { rowid: 2 }));
    }

    #[test]
    fn deleting_the_only_row_leaves_an_empty_root_leaf() {
        let page_size = 512u32;
        let (vfs, header) = minimal_db(page_size);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();

        insert_row(&mut pager, &header, 1, 1, b"hello").unwrap();
        delete_row(&mut pager, &header, 1, 1).unwrap();

        let header_start = page1_header_start(1);
        let buf = pager.get_page_mut(1).unwrap().clone();
        let page_type = read_page_type(&buf, header_start, 1).unwrap();
        assert_eq!(page_type, LEAF_TABLE);
        let cells = collect_leaf_cells(&buf, header_start, 1, header.usable_page_size()).unwrap();
        assert!(cells.is_empty());
    }

    #[test]
    fn deleting_one_of_two_rows_keeps_the_other() {
        let page_size = 512u32;
        let (vfs, header) = minimal_db(page_size);
        let mut pager = Pager::open(&vfs, Path::new("/test.db"), page_size).unwrap();

        insert_row(&mut pager, &header, 1, 1, b"hello").unwrap();
        insert_row(&mut pager, &header, 1, 2, b"world").unwrap();
        delete_row(&mut pager, &header, 1, 1).unwrap();

        let header_start = page1_header_start(1);
        let buf = pager.get_page_mut(1).unwrap().clone();
        let cells = collect_leaf_cells(&buf, header_start, 1, header.usable_page_size()).unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].0, 2);
    }
}
