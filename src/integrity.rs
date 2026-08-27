// Copyright 2026 Schuberg Philis
// SPDX-License-Identifier: Apache-2.0
//! `PRAGMA integrity_check`/`quick_check` (#540, #541): walks every table
//! and index b-tree plus the freelist chain, reporting structural
//! problems in the same textual shape stock `sqlite3` uses -- a single
//! `"ok"` row when nothing is wrong, or one row per problem found.
//!
//! Scope: table-b-tree rowid ordering, index-b-tree key ordering, and
//! (for `integrity_check` only, not `quick_check`) the index-vs-table
//! cross-check -- every index entry's trailing rowid must exist in its
//! table, and every table row must be represented by every one of its
//! table's indexes. Also walks the freelist chain and checks its page
//! count against the header. Pointer-map cross-validation is out of
//! scope: this crate never writes a pointer-map (see `src/pager.rs`'s
//! module doc) since it has no auto-vacuum/incremental-vacuum support,
//! so a `largest_root_btree_page != 0` database (auto-vacuum, only ever
//! seen in externally-crafted fixtures) is reported as a single
//! informational problem rather than silently mis-validated.

use std::collections::HashSet;

use crate::btree::{IndexCursor, IndexRow, TableCursor};
use crate::header::DatabaseHeader;
use crate::pager::freelist::TrunkPage;
use crate::record::{decode_record, Value};
use crate::schema::{read_schema, IndexSchema, TableSchema};
use crate::vfs::PageSource;

/// Runs the check and returns `["ok"]` if nothing is wrong, or one
/// human-readable problem description per row otherwise. `quick` skips
/// the index-vs-table cross-check pass (`PRAGMA quick_check`, #541).
/// Generic over `P` (rather than a `dyn PageSource` trait object) to stay
/// inside the `mvl-limit` qualified subset (see `src/pager.rs`'s module
/// doc: only `src/vfs/` and the VDBE's `Rc<dyn PageSource>` boundary in
/// `src/vdbe/{exec,cursor}.rs` are exempt) -- the caller in
/// `src/vdbe/pragma.rs` passes its own `Rc<dyn PageSource>` as `P`.
pub fn run_integrity_check<P: PageSource + Clone>(
    source: P,
    header: &DatabaseHeader,
    quick: bool,
) -> Vec<String> {
    let mut problems = Vec::new();

    if header.largest_root_btree_page != 0 {
        problems.push(
            "auto-vacuum database: pointer-map cross-check is not implemented, skipped".to_string(),
        );
    }

    let mut master_cursor = TableCursor::new(source.clone(), header, 1);
    let schemas = match read_schema(&mut master_cursor, header.text_encoding) {
        Ok(s) => s,
        Err(e) => {
            problems.push(format!("*** in database main *** sqlite_master: {e}"));
            return problems;
        }
    };

    for table in &schemas {
        if table.is_virtual {
            continue;
        }
        let table_rowids = check_table(&source, header, table, &mut problems);
        if !quick {
            for index in &table.indexes {
                check_index(&source, header, table, index, &table_rowids, &mut problems);
            }
        }
    }

    check_freelist(&source, header, &mut problems);

    if problems.is_empty() {
        vec!["ok".to_string()]
    } else {
        problems
    }
}

/// Walks `table`'s b-tree via [`TableCursor`], checking rowids are
/// strictly increasing (the on-disk invariant every table b-tree must
/// satisfy). Returns the set of rowids seen, for the index cross-check.
fn check_table<P: PageSource + Clone>(
    source: &P,
    header: &DatabaseHeader,
    table: &TableSchema,
    problems: &mut Vec<String>,
) -> HashSet<i64> {
    let mut rowids = HashSet::new();
    let mut cursor = TableCursor::new(source.clone(), header, table.root_page);
    let mut prev: Option<i64> = None;
    let mut row = match cursor.first() {
        Ok(r) => r,
        Err(e) => {
            problems.push(format!(
                "*** in database main *** table {:?}: {e}",
                table.name
            ));
            return rowids;
        }
    };
    while let Some(rowid) = row {
        if let Some(p) = prev {
            if rowid <= p {
                problems.push(format!(
                    "*** in database main *** table {:?}: rowid {rowid} out of order after {p}",
                    table.name
                ));
            }
        }
        if !rowids.insert(rowid) {
            problems.push(format!(
                "*** in database main *** table {:?}: duplicate rowid {rowid}",
                table.name
            ));
        }
        prev = Some(rowid);
        row = match cursor.next() {
            Ok(r) => r,
            Err(e) => {
                problems.push(format!(
                    "*** in database main *** table {:?}: {e}",
                    table.name
                ));
                None
            }
        };
    }
    rowids
}

/// Walks `index`'s b-tree via [`IndexCursor`], checking key ordering and
/// (the "exhaustive" part `quick_check` skips) cross-checking every
/// decoded entry's trailing rowid against `table_rowids`, plus that the
/// index has exactly as many entries as the table has rows.
fn check_index<P: PageSource + Clone>(
    source: &P,
    header: &DatabaseHeader,
    table: &TableSchema,
    index: &IndexSchema,
    table_rowids: &HashSet<i64>,
    problems: &mut Vec<String>,
) {
    let mut cursor = IndexCursor::new(source.clone(), header.usable_page_size(), index.root_page);
    let mut prev_key: Option<Vec<Value>> = None;
    let mut seen = 0usize;
    let mut row = match cursor.first() {
        Ok(r) => r,
        Err(e) => {
            problems.push(format!(
                "*** in database main *** index {:?}: {e}",
                index.name
            ));
            return;
        }
    };
    while let Some(entry) = row {
        let decoded = match decode_record(&entry.payload, header.text_encoding) {
            Ok(v) => v,
            Err(e) => {
                problems.push(format!(
                    "*** in database main *** index {:?}: malformed entry: {e}",
                    index.name
                ));
                row = advance(&mut cursor, index, problems);
                continue;
            }
        };
        let Some(Value::Integer(rowid)) = decoded.last() else {
            problems.push(format!(
                "*** in database main *** index {:?}: entry has no trailing rowid",
                index.name
            ));
            row = advance(&mut cursor, index, problems);
            continue;
        };
        if !table_rowids.contains(rowid) {
            problems.push(format!(
                "*** in database main *** index {:?}: entry references rowid {rowid} not present in table {:?}",
                index.name, table.name
            ));
        }
        let key = decoded
            .get(..decoded.len().saturating_sub(1))
            .unwrap_or(&[]);
        if let Some(p) = &prev_key {
            if compare_index_keys(p.as_slice(), key) == std::cmp::Ordering::Greater {
                problems.push(format!(
                    "*** in database main *** index {:?}: keys out of order",
                    index.name
                ));
            }
        }
        prev_key = Some(key.to_vec());
        seen = seen.saturating_add(1);
        row = advance(&mut cursor, index, problems);
    }
    if seen != table_rowids.len() {
        problems.push(format!(
            "*** in database main *** wrong # of entries in index {:?}: expected {}, found {seen}",
            index.name,
            table_rowids.len()
        ));
    }
}

fn advance<P: PageSource>(
    cursor: &mut IndexCursor<P>,
    index: &IndexSchema,
    problems: &mut Vec<String>,
) -> Option<IndexRow> {
    match cursor.next() {
        Ok(r) => r,
        Err(e) => {
            problems.push(format!(
                "*** in database main *** index {:?}: {e}",
                index.name
            ));
            None
        }
    }
}

/// Lexicographic comparison over a decoded (possibly composite) index
/// key, `BINARY`-collation only -- per-column `COLLATE` is not applied
/// here (known limitation; matches this checker's scope, not stock
/// SQLite's full collation-aware `integrity_check`).
fn compare_index_keys(a: &[Value], b: &[Value]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let c = compare_values(x, y);
        if c != std::cmp::Ordering::Equal {
            return c;
        }
    }
    a.len().cmp(&b.len())
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
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
        _ => Ordering::Equal,
    }
}

/// Walks the freelist trunk chain from `header.freelist_trunk_page`,
/// checking that the number of leaf pages visited matches
/// `header.freelist_page_count` and that no page number repeats or is
/// out of range.
fn check_freelist<P: PageSource>(source: &P, header: &DatabaseHeader, problems: &mut Vec<String>) {
    if header.freelist_trunk_page == 0 {
        if header.freelist_page_count != 0 {
            problems.push(format!(
                "freelist_page_count is {} but there is no freelist trunk page",
                header.freelist_page_count
            ));
        }
        return;
    }
    let mut seen_trunks = HashSet::new();
    let mut total_leaves = 0u32;
    let mut trunk = header.freelist_trunk_page;
    let max_hops = header.page_count.saturating_add(1);
    let mut hops = 0u32;
    while trunk != 0 {
        hops = hops.saturating_add(1);
        if hops > max_hops {
            problems.push(
                "freelist trunk chain longer than the database's page count (cycle?)".to_string(),
            );
            break;
        }
        if trunk > header.page_count || !seen_trunks.insert(trunk) {
            problems.push(format!(
                "freelist trunk page {trunk} is out of range or repeated"
            ));
            break;
        }
        let buf = match source.read_page(trunk) {
            Ok(b) => b,
            Err(e) => {
                problems.push(format!("reading freelist trunk page {trunk}: {e}"));
                break;
            }
        };
        let page = match TrunkPage::parse(&buf) {
            Ok(p) => p,
            Err(e) => {
                problems.push(format!("parsing freelist trunk page {trunk}: {e}"));
                break;
            }
        };
        for leaf in &page.leaves {
            if *leaf == 0 || *leaf > header.page_count {
                problems.push(format!("freelist leaf page {leaf} is out of range"));
            }
        }
        total_leaves = total_leaves.saturating_add(page.leaves.len() as u32);
        trunk = page.next_trunk;
    }
    let total = total_leaves.saturating_add(seen_trunks.len() as u32);
    if total != header.freelist_page_count {
        problems.push(format!(
            "freelist_page_count is {} but the trunk chain has {total} pages",
            header.freelist_page_count
        ));
    }
}
