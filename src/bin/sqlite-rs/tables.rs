//! `tables <file> [PATTERN]`: `sqlite3`'s `.tables [PATTERN]` shell
//! command (#177) — table and view names from `sqlite_master`, sorted
//! alphabetically, excluding internal `sqlite_%` names, optionally
//! filtered by a LIKE `PATTERN`, rendered in `.tables`'s multi-column
//! layout. Temp-table `temp.` prefixing is deferred (needs the V3+ write
//! path's temp-database support).

use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;

use sqlite_rs::btree::TableCursor;
use sqlite_rs::dump;
use sqlite_rs::schema::read_table_and_view_names;
use sqlite_rs::vdbe::like_match;
use sqlite_rs::vfs::{PageSource, UnixVfs};

use crate::common::fatal;

pub fn run_tables(path: &Path, pattern: Option<&str>) -> ExitCode {
    let (header, pager) = match dump::open(&UnixVfs, path) {
        Ok(v) => v,
        Err(e) => return fatal(path, &e),
    };
    let source: Rc<dyn PageSource> = Rc::new(pager);

    let mut schema_cursor = TableCursor::new(Rc::clone(&source), &header, 1);
    let names = match read_table_and_view_names(&mut schema_cursor, header.text_encoding) {
        Ok(n) => n,
        Err(e) => return fatal(path, &e),
    };

    let mut names: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| !name.starts_with("sqlite_"))
        .filter(|name| pattern.is_none_or(|p| like_match(name, p, None)))
        .collect();
    names.sort_unstable();

    print_columnized(&names);
    ExitCode::SUCCESS
}

/// The shell's assumed terminal width for `.tables`' column-fitting search.
const TABLES_TERM_WIDTH: usize = 80;

/// Per-column gap (verified empirically against the pinned 3.53.4 oracle:
/// each column is left-padded to `column's longest name + 5`, not the
/// more common `+2` — e.g. `.tables` on a 2-table db prints
/// `ab     verylongtablename123` with a 5-space gap after `ab`).
const TABLES_COLUMN_GAP: usize = 5;

/// The longest name in `names[start..end]` (`0` for an empty range).
fn column_max_len(names: &[&str], start: usize, end: usize) -> usize {
    names
        .get(start..end.min(names.len()))
        .into_iter()
        .flatten()
        .map(|n| n.len())
        .max()
        .unwrap_or(0)
}

/// Total row width column-major layout with `num_cols` columns would take,
/// per-column width `TABLES_COLUMN_GAP` narrower for whichever column ends
/// up rightmost overall (no trailing padding needed there).
fn row_width(names: &[&str], num_cols: usize, num_rows: usize) -> usize {
    let last_idx = names.len().saturating_sub(1);
    let mut total = 0usize;
    for col in 0..num_cols {
        let start = col.saturating_mul(num_rows);
        if start >= names.len() {
            break;
        }
        let end = start.saturating_add(num_rows);
        let colmax = column_max_len(names, start, end);
        if start.saturating_add(num_rows) > last_idx {
            total = total.saturating_add(colmax);
        } else {
            total = total
                .saturating_add(colmax)
                .saturating_add(TABLES_COLUMN_GAP);
        }
    }
    total
}

/// Renders `names` the way `sqlite3`'s shell prints `.tables` output:
/// column-major (fill down each column before moving to the next), each
/// column padded to that column's own longest name (see
/// [`TABLES_COLUMN_GAP`]), wrapped to fit [`TABLES_TERM_WIDTH`] columns —
/// picks the widest column count whose row width still fits.
fn print_columnized(names: &[&str]) {
    if names.is_empty() {
        return;
    }
    let num_cols = (1..=names.len())
        .rev()
        .find(|&nc| {
            let nr = names.len().div_ceil(nc);
            row_width(names, nc, nr) <= TABLES_TERM_WIDTH
        })
        .unwrap_or(1);
    let num_rows = names.len().div_ceil(num_cols);

    let col_widths: Vec<usize> = (0..num_cols)
        .map(|col| {
            let start = col.saturating_mul(num_rows);
            column_max_len(names, start, start.saturating_add(num_rows))
        })
        .collect();

    for row in 0..num_rows {
        let mut line = String::new();
        for (col, col_width) in col_widths.iter().enumerate() {
            let idx = col.saturating_mul(num_rows).saturating_add(row);
            let Some(name) = names.get(idx) else {
                continue;
            };
            if idx.saturating_add(num_rows) >= names.len() {
                line.push_str(name);
            } else {
                let width = col_width.saturating_add(TABLES_COLUMN_GAP);
                line.push_str(&format!("{name:<width$}"));
            }
        }
        println!("{line}");
    }
}
